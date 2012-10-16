use au = gfx::geometry;
use core::dlist::DList;
use core::dvec::DVec;
use css::values::{BoxAuto, BoxLength, Px};
use dl = gfx::display_list;
use dom::node::Node;
use geom::point::Point2D;
use geom::rect::Rect;
use geom::size::Size2D;
use gfx::geometry::au;
use layout::box::*;
use layout::context::LayoutContext;
use layout::flow::{FlowContext, InlineFlow};
use layout::text::TextBoxData;
use num::Num;
use servo_text::text_run::TextRun;
use servo_text::util::*;
use std::arc;
use util::tree;

/*
Lineboxes are represented as offsets into the child list, rather than
as an object that "owns" boxes. Choosing a different set of line
breaks requires a new list of offsets, and possibly some splitting and
merging of TextBoxes.

A similar list will keep track of the mapping between CSS boxes and
the corresponding render boxes in the inline flow.

After line breaks are determined, render boxes in the inline flow may
overlap visually. For example, in the case of nested inline CSS boxes,
outer inlines must be at least as large as the inner inlines, for
purposes of drawing noninherited things like backgrounds, borders,
outlines.

N.B. roc has an alternative design where the list instead consists of
things like "start outer box, text, start inner box, text, end inner
box, text, end outer box, text". This seems a little complicated to
serve as the starting point, but the current design doesn't make it
hard to try out that alternative.
*/

type BoxRange = {mut start: u16, mut len: u16};
fn EmptyBoxRange() -> BoxRange {
    { mut start: 0 as u16, mut len: 0 as u16 }
}

type NodeRange = {node: Node, span: BoxRange};

// stack-allocated object for scanning an inline flow into
// TextRun-containing TextBoxes.
struct TextRunScanner {
    mut in_clump: bool,
    mut clump_start: uint,
    mut clump_end: uint,
    flow: @FlowContext,
}

fn TextRunScanner(flow: @FlowContext) -> TextRunScanner {
    TextRunScanner {
        in_clump: false,
        clump_start: 0,
        clump_end: 0,
        flow: flow,
    }
}

impl TextRunScanner {
    fn scan_for_runs(ctx: &LayoutContext) {
        // if reused, must be reset.
        assert !self.in_clump;
        assert self.flow.inline().boxes.len() > 0;

        do self.flow.inline().boxes.swap |in_boxes| {
            debug!("TextRunScanner: scanning %u boxes for text runs...", in_boxes.len());
            
            let out_boxes = DVec();
            let mut prev_box: @RenderBox = in_boxes[0];

            for uint::range(0, in_boxes.len()) |i| {
                debug!("TextRunScanner: considering box: %?", in_boxes[i].debug_str());

                let can_coalesce_with_prev = i > 0 && boxes_can_be_coalesced(prev_box, in_boxes[i]);

                match (self.in_clump, can_coalesce_with_prev) {
                    // start a new clump
                    (false, _)    => { self.reset_clump_to_index(i); },
                    // extend clump
                    (true, true)  => { self.clump_end = i; },
                    // boundary detected; flush and start new clump
                    (true, false) => {
                        self.flush_clump_to_list(ctx, in_boxes, &out_boxes);
                        self.reset_clump_to_index(i);
                    }
                };
                
                prev_box = in_boxes[i];
            }
            // handle remaining clumps
            if self.in_clump {
                self.flush_clump_to_list(ctx, in_boxes, &out_boxes);
            }

            debug!("TextRunScanner: swapping out boxes.");
            // swap out old and new box list of flow, by supplying
            // temp boxes as return value to boxes.swap |...|
            dvec::unwrap(out_boxes)
        }

        // helper functions
        pure fn boxes_can_be_coalesced(a: @RenderBox, b: @RenderBox) -> bool {
            assert !core::box::ptr_eq(a, b);

            match (a, b) {
                // TODO(Issue #117): check whether text styles, fonts are the same.
                (@UnscannedTextBox(*), @UnscannedTextBox(*)) => a.can_merge_with_box(b),
                (_, _) => false
            }
        }
    }

    fn reset_clump_to_index(i: uint) {
        debug!("TextRunScanner: resetting clump to %u", i);
        
        self.clump_start = i;
        self.clump_end = i;
        self.in_clump = true;
    }

    // a 'clump' is a range of inline flow leaves that can be merged
    // together into a single RenderBox. Adjacent text with the same
    // style can be merged, and nothing else can. 
    //
    // the flow keeps track of the RenderBoxes contained by all
    // non-leaf DOM nodes. This is necessary for correct painting
    // order. Since we compress several leaf RenderBoxes here, the
    // mapping must be adjusted.
    // 
    // N.B. in_boxes is passed by reference, since we cannot
    // recursively borrow or swap the flow's dvec of boxes. When all
    // boxes are appended, the caller swaps the flow's box list.
    fn flush_clump_to_list(ctx: &LayoutContext, 
                           in_boxes: &[@RenderBox], out_boxes: &DVec<@RenderBox>) {
        assert self.in_clump;

        debug!("TextRunScanner: flushing boxes when start=%u,end=%u",
               self.clump_start, self.clump_end);

        let is_singleton = (self.clump_start == self.clump_end);
        let is_text_clump = match in_boxes[self.clump_start] {
            @UnscannedTextBox(*) => true,
            _ => false
        };

        match (is_singleton, is_text_clump) {
            (false, false) => fail ~"WAT: can't coalesce non-text boxes in flush_clump_to_list()!",
            (true, false) => { 
                debug!("TextRunScanner: pushing single non-text box when start=%u,end=%u", 
                       self.clump_start, self.clump_end);
                out_boxes.push(in_boxes[self.clump_start]);
            },
            (true, true)  => { 
                let text = in_boxes[self.clump_start].raw_text();
                // TODO(Issue #115): use actual CSS 'white-space' property of relevant style.
                let compression = CompressWhitespaceNewline;
                let transformed_text = transform_text(text, compression);
                // TODO(Issue #116): use actual font for corresponding DOM node to create text run.
                let run = @TextRun(ctx.font_cache.get_test_font(), move transformed_text);
                debug!("TextRunScanner: pushing single text box when start=%u,end=%u",
                       self.clump_start, self.clump_end);
                let new_box = layout::text::adapt_textbox_with_range(in_boxes[self.clump_start].d(), run, 0, run.text.len());
                out_boxes.push(new_box);
            },
            (false, true) => {
                // TODO(Issue #115): use actual CSS 'white-space' property of relevant style.
                let compression = CompressWhitespaceNewline;
                let clump_box_count = self.clump_end - self.clump_start + 1;

                // first, transform/compress text of all the nodes
                let transformed_strs : ~[~str] = vec::from_fn(clump_box_count, |i| {
                    // TODO(Issue #113): we shoud be passing compression context
                    // between calls to transform_text, so that boxes
                    // starting/ending with whitespace &c can be
                    // compressed correctly w.r.t. the TextRun.
                    let idx = i + self.clump_start;
                    transform_text(in_boxes[idx].raw_text(), compression)
                });

                // then, fix NodeRange mappings to account for elided boxes.
                do self.flow.inline().elems.borrow |ranges: &[NodeRange]| {
                    for ranges.each |range: &NodeRange| {
                        let span = &range.span;
                        let relation = relation_of_clump_and_range(span, self.clump_start, self.clump_end);
                        debug!("TextRunScanner: possibly repairing element range %?", range.span);
                        debug!("TextRunScanner: relation of range and clump(start=%u, end=%u): %?",
                               self.clump_start, self.clump_end, relation);
                        match relation {
                            RangeEntirelyBeforeClump => {},
                            RangeEntirelyAfterClump => { span.start -= clump_box_count as u16; },
                            RangeCoincidesClump | RangeContainedByClump => 
                                                       { span.start  = self.clump_start as u16;
                                                         span.len    = 1 },
                            RangeContainsClump =>      { span.len   -= clump_box_count as u16; },
                            RangeOverlapsClumpStart(overlap) => 
                                                       { span.len   -= (overlap - 1) as u16; },
                            RangeOverlapsClumpEnd(overlap) => 
                                                       { span.start  = self.clump_start as u16;
                                                         span.len   -= (overlap - 1) as u16; }
                        }
                        debug!("TextRunScanner: new element range: ---- %?", range.span);
                    }
                }

                // TODO(Issue #118): use a rope, simply give ownership of  nonzero strs to rope
                let mut run_str : ~str = ~"";
                for uint::range(0, transformed_strs.len()) |i| {
                    str::push_str(&mut run_str, transformed_strs[i]);
                }
                
                // TODO(Issue #116): use actual font for corresponding DOM node to create text run.
                let run = @TextRun(ctx.font_cache.get_test_font(), move run_str);
                debug!("TextRunScanner: pushing box(es) when start=%u,end=%u",
                       self.clump_start, self.clump_end);
                let new_box = layout::text::adapt_textbox_with_range(in_boxes[self.clump_start].d(), run, 0, run.text.len());
                out_boxes.push(new_box);
            }
        } /* /match */
        self.in_clump = false;
    
        enum ClumpRangeRelation {
            RangeOverlapsClumpStart(/* overlap */ uint),
            RangeOverlapsClumpEnd(/* overlap */ uint),
            RangeContainedByClump,
            RangeContainsClump,
            RangeCoincidesClump,
            RangeEntirelyBeforeClump,
            RangeEntirelyAfterClump
        }
        
        fn relation_of_clump_and_range(range: &BoxRange, clump_start: uint, 
                                       clump_end: uint) -> ClumpRangeRelation {
            let range_start = range.start as uint;
            let range_end = (range.start + range.len) as uint;

            if range_end < clump_start {
                return RangeEntirelyBeforeClump;
            } 
            if range_start > clump_end {
                return RangeEntirelyAfterClump;
            }
            if range_start == clump_start && range_end == clump_end {
                return RangeCoincidesClump;
            }
            if range_start <= clump_start && range_end >= clump_end {
                return RangeContainsClump;
            }
            if range_start >= clump_start && range_end <= clump_end {
                return RangeContainedByClump;
            }
            if range_start < clump_start && range_end < clump_end {
                let overlap = range_end - clump_start;
                return RangeOverlapsClumpStart(overlap);
            }
            if range_start > clump_start && range_end > clump_end {
                let overlap = clump_end - range_start;
                return RangeOverlapsClumpEnd(overlap);
            }
            fail fmt!("relation_of_clump_and_range(): didn't classify range=%?, clump_start=%u, clump_end=%u",
                      range, clump_start, clump_end);
        }
    } /* /fn flush_clump_to_list */
}

struct LineboxScanner {
    flow: @FlowContext,
    new_boxes: DVec<@RenderBox>,
    work_list: DList<@RenderBox>,
    mut pending_line: {span: BoxRange, width: au},
    line_spans: DVec<BoxRange>
}

fn LineboxScanner(inline: @FlowContext) -> LineboxScanner {
    assert inline.starts_inline_flow();

    LineboxScanner {
        flow: inline,
        new_boxes: DVec(),
        work_list: DList(),
        pending_line: {span: EmptyBoxRange(), width: au(0)},
        line_spans: DVec()
    }
}

impl LineboxScanner {
    fn reset() {
        debug!("Resetting line box scanner's state for flow f%d.", self.flow.d().id);
        do self.line_spans.swap |_v| { ~[] };
        do self.new_boxes.swap |_v| { ~[] };
        self.pending_line = {span: EmptyBoxRange(), width: au(0)};
    }

    pub fn scan_for_lines(ctx: &LayoutContext) {
        self.reset();
        
        let boxes = &self.flow.inline().boxes;
        let mut i = 0u;

        loop {
            // acquire the next box to lay out from work list or box list
            let cur_box = match (self.work_list.pop(), boxes.len()) {
                (Some(box), _) => { 
                    debug!("LineboxScanner: Working with box from work list: b%d", box.d().id);
                    box
                },
                (None, 0) => { break },
                (None, _) => { 
                    let box = boxes[i]; i += 1;
                    debug!("LineboxScanner: Working with box from box list: b%d", box.d().id);
                    box
                }
            };

            let box_was_appended = self.try_append_to_line(ctx, cur_box);
            if !box_was_appended {
                debug!("LineboxScanner: Box wasn't appended, because line %u was full.",
                       self.line_spans.len());
                self.flush_current_line();
            }
        }

        if self.pending_line.span.len > 0 {
            debug!("LineboxScanner: Partially full linebox %u left at end of scanning.",
                   self.line_spans.len());
            self.flush_current_line();
        }

        // cleanup and store results
        debug!("LineboxScanner: Propagating scanned lines[n=%u] to inline flow f%d", 
               self.line_spans.len(), self.flow.d().id);
        // TODO: repair Node->box mappings, using strategy similar to TextRunScanner
        do self.new_boxes.swap |boxes| {
            self.flow.inline().boxes.set(boxes);
            ~[]
        };
        do self.line_spans.swap |boxes| {
            self.flow.inline().lines.set(boxes);
            ~[]
        };
    }

    priv fn flush_current_line() {
        debug!("LineboxScanner: Flushing line %u: %?",
               self.line_spans.len(), self.pending_line);
        // set box horizontal offsets
        let boxes = &self.flow.inline().boxes;
        let line_span = copy self.pending_line.span;
        let mut offset_x = au(0);
        // TODO: interpretation of CSS 'text-direction' and 'text-align' 
        // will change from which side we start laying out the line.
        debug!("LineboxScanner: Setting horizontal offsets for boxes in line %u range: %?",
               self.line_spans.len(), line_span);
        for uint::range(line_span.start as uint, (line_span.start + line_span.len) as uint) |i| {
            let box_data = &boxes[i].d();
            box_data.position.origin.x = offset_x;
            offset_x += box_data.position.size.width;
        }

        // clear line and add line mapping
        debug!("LineboxScanner: Saving information for flushed line %u.", self.line_spans.len());
        self.line_spans.push(copy self.pending_line.span);
        self.pending_line = {span: EmptyBoxRange(), width: au(0)};
    }

    // return value: whether any box was appended.
    priv fn try_append_to_line(ctx: &LayoutContext, in_box: @RenderBox) -> bool {
        let remaining_width = self.flow.d().position.size.width - self.pending_line.width;
        let in_box_width = in_box.d().position.size.width;
        let line_is_empty: bool = self.pending_line.span.len == 0;

        debug!("LineboxScanner: Trying to append box to line %u (box width: %?, remaining width: %?): %s",
               self.line_spans.len(), in_box_width, remaining_width, in_box.debug_str());

        if in_box_width <= remaining_width {
            debug!("LineboxScanner: case=box fits without splitting");
            self.push_box_to_line(in_box);
            return true;
        }

        if !in_box.can_split() {
            // force it onto the line anyway, if its otherwise empty
            // TODO: signal that horizontal overflow happened?
            if line_is_empty {
                debug!("LineboxScanner: case=box can't split and line %u is empty, so overflowing.",
                      self.line_spans.len());
                self.push_box_to_line(in_box);
                return true;
            } else {
                debug!("LineboxScanner: Case=box can't split, not appending.");
                return false;
            }
        }

        // not enough width; try splitting?
        match in_box.split_to_width(ctx, remaining_width, line_is_empty) {
            CannotSplit(_) => {
                error!("LineboxScanner: Tried to split unsplittable render box! %s", in_box.debug_str());
                return false;
            },
            SplitUnnecessary(_) => {
                error!("LineboxScanner: Tried to split when un-split piece was small enough! %s", in_box.debug_str());
                self.push_box_to_line(in_box);
                return true;
            },
            SplitDidFit(left, right) => {
                debug!("LineboxScanner: case=split box did fit; deferring remainder box.");
                self.push_box_to_line(left);
                self.work_list.push_head(right);
                return true;
            },
            SplitDidNotFit(left, right) => {
                if line_is_empty {
                    debug!("LineboxScanner: case=split box didn't fit and line %u is empty, so overflowing and deferring remainder box.",
                          self.line_spans.len());
                    // TODO: signal that horizontal overflow happened?
                    self.push_box_to_line(left);
                    self.work_list.push_head(right);
                    return true;
                } else {
                    debug!("LineboxScanner: case=split box didn't fit, not appending and deferring original box.");
                    self.work_list.push_head(in_box);
                    return false;
                }
            }
        }
    }

    // unconditional push
    priv fn push_box_to_line(box: @RenderBox) {
        debug!("LineboxScanner: Pushing box b%d to line %u", box.d().id, self.line_spans.len());

        if self.pending_line.span.len == 0 {
            assert self.new_boxes.len() <= (core::u16::max_value as uint);
            self.pending_line.span.start = self.new_boxes.len() as u16;
        }
        self.pending_line.span.len += 1;
        self.pending_line.width += box.d().position.size.width;
        self.new_boxes.push(box);
    }
}

struct InlineFlowData {
    // A vec of all inline render boxes. Several boxes may
    // correspond to one Node/Element.
    boxes: DVec<@RenderBox>,
    // vec of ranges into boxes that represents line positions.
    // these ranges are disjoint, and are the result of inline layout.
    lines: DVec<BoxRange>,
    // vec of ranges into boxes that represent elements. These ranges
    // must be well-nested, and are only related to the content of
    // boxes (not lines). Ranges are only kept for non-leaf elements.
    elems: DVec<NodeRange>
}

fn InlineFlowData() -> InlineFlowData {
    InlineFlowData {
        boxes: DVec(),
        lines: DVec(),
        elems: DVec(),
    }
}

trait InlineLayout {
    pure fn starts_inline_flow() -> bool;

    fn bubble_widths_inline(@self, ctx: &LayoutContext);
    fn assign_widths_inline(@self, ctx: &LayoutContext);
    fn assign_height_inline(@self, ctx: &LayoutContext);
    fn build_display_list_inline(@self, a: &dl::DisplayListBuilder, b: &Rect<au>, c: &Point2D<au>, d: &dl::DisplayList);
}

impl FlowContext : InlineLayout {
    pure fn starts_inline_flow() -> bool { match self { InlineFlow(*) => true, _ => false } }

    fn bubble_widths_inline(@self, ctx: &LayoutContext) {
        assert self.starts_inline_flow();

        let scanner = TextRunScanner(self);
        scanner.scan_for_runs(ctx);

        let mut min_width = au(0);
        let mut pref_width = au(0);

        for self.inline().boxes.each |box| {
            debug!("FlowContext[%d]: measuring %s", self.d().id, box.debug_str());
            min_width = au::max(min_width, box.get_min_width(ctx));
            pref_width = au::max(pref_width, box.get_pref_width(ctx));
        }

        self.d().min_width = min_width;
        self.d().pref_width = pref_width;
    }

    /* Recursively (top-down) determines the actual width of child
    contexts and boxes. When called on this context, the context has
    had its width set by the parent context. */
    fn assign_widths_inline(@self, _ctx: &LayoutContext) {
        assert self.starts_inline_flow();

        // initialize (content) box widths, if they haven't been
        // already. This could be combined with LineboxScanner's walk
        // over the box list, and/or put into RenderBox.
        for self.inline().boxes.each |box| {
            box.d().position.size.width = match *box {
                @ImageBox(_,img) => au::from_px(img.get_size().get_default(Size2D(0,0)).width),
                @TextBox(*) => { /* text boxes are initialized with dimensions */
                                   box.d().position.size.width
                },
                @GenericBox(*) => au::from_px(45), /* TODO: should use CSS 'width'? */
                _ => fail fmt!("Tried to assign width to unknown Box variant: %?", box)
            };
        } // for boxes.each |box|

        //let scanner = LineBoxScanner(self);
        //scanner.scan_for_lines(ctx);
   
        /* There are no child contexts, so stop here. */

        // TODO: once there are 'inline-block' elements, this won't be
        // true.  In that case, set the InlineBlockBox's width to the
        // shrink-to-fit width, perform inline flow, and set the block
        // flow context's width as the assigned width of the
        // 'inline-block' box that created this flow before recursing.
    }

    fn assign_height_inline(@self, _ctx: &LayoutContext) {
        // TODO: calculate linebox heights and set y-offsets
        let line_height = au::from_px(20);
        let mut cur_y = au(0);

        for self.inline().boxes.each |box| {
            let box_height = match *box {
                @ImageBox(_,img) => au::from_px(img.size().height),
                @TextBox(*) => { /* text boxes are initialized with dimensions */
                                 box.d().position.size.height
                },
                @GenericBox(*) => au::from_px(30), /* TODO: should use CSS 'height'? */
                _ => fail fmt!("Tried to assign width to unknown Box variant: %?", box)
            };
            // TODO: calculate linebox heights and set y-offsets
            box.d().position.origin.y = cur_y;
            cur_y += au::max(line_height, box_height);
            box.d().position.size.height = box_height;
        } // for boxes.each |box|

        self.d().position.size.height = cur_y;
    }

    fn build_display_list_inline(@self, builder: &dl::DisplayListBuilder, dirty: &Rect<au>, 
                                 offset: &Point2D<au>, list: &dl::DisplayList) {

        assert self.starts_inline_flow();

        // TODO: if the CSS box introducing this inline context is *not* anonymous,
        // we need to draw it too, in a way similar to BlockFlowContext

        // TODO: once we form line boxes and have their cached bounds, we can be 
        // smarter and not recurse on a line if nothing in it can intersect dirty
        debug!("FlowContext[%d]: building display list for %u inline boxes",
               self.d().id, self.inline().boxes.len());
        for self.inline().boxes.each |box| {
            box.build_display_list(builder, dirty, offset, list)
        }

        // TODO: should inline-block elements have flows as children
        // of the inline flow, or should the flow be nested inside the
        // box somehow? Maybe it's best to unify flows and boxes into
        // the same enum, so inline-block flows are normal
        // (indivisible) children in the inline flow child list.
    }

} // @FlowContext : InlineLayout
