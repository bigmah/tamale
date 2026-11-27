//! tamale's component library: each component's positional signature and what it's for.
//! The renderer reads arguments by these names ([`crate::Node::arg`]), and the prompt's
//! reference ([`reference`]) is written from the same table, so the two can't drift apart.

pub struct Param {
    pub name: &'static str,
    /// The type as the prompt shows it.
    pub ty: &'static str,
    pub required: bool,
}

pub struct Component {
    pub name: &'static str,
    pub group: &'static str,
    pub params: &'static [Param],
    pub doc: &'static str,
}

const fn p(name: &'static str, ty: &'static str) -> Param {
    Param {
        name,
        ty,
        required: true,
    }
}

const fn o(name: &'static str, ty: &'static str) -> Param {
    Param {
        name,
        ty,
        required: false,
    }
}

macro_rules! c {
    ($group:literal, $name:literal, [$($param:expr),* $(,)?], $doc:literal) => {
        Component { name: $name, group: $group, params: &[$($param),*], doc: $doc }
    };
}

pub const GROUPS: &[(&str, &str)] = &[
    (
        "Layout",
        "Structure. Stacks and grids collapse to one column on phones by themselves.",
    ),
    (
        "Content",
        "Text, code and data. Text arguments take inline markdown: **bold**, *italic*, `code`, [links](https://…).",
    ),
    ("Charts", "Drawn as SVG, sized to the width."),
    (
        "Interactive",
        "Controls. An action is `@ToAssistant(\"message\", context?)` (sends the user's message to you), `@OpenUrl(url)`, `@Set($var, value)`, `@Reset($var, …)`, or several in `Action([…])`.",
    ),
    (
        "Lessons",
        "Only for a lesson (see Lessons): `root = Lesson(…)`, never inside a Card. tamale shows one part at a time with the reader's progress, sends their answers and questions to you, and shows your feedback in the part they came from.",
    ),
];

pub const LIBRARY: &[Component] = &[
    // ---------------------------------------------------------------- layout
    c!(
        "Layout",
        "Stack",
        [
            p("children", "Node[]"),
            o("direction", "\"column\" | \"row\""),
            o("gap", "\"s\" | \"m\" | \"l\""),
            o(
                "align",
                "\"start\" | \"center\" | \"end\" | \"stretch\" | \"between\""
            )
        ],
        "Children in a column (default) or a row; rows wrap."
    ),
    c!(
        "Layout",
        "Grid",
        [p("children", "Node[]"), o("minWidth", "number")],
        "Responsive grid of equal columns, each at least minWidth px (default 220)."
    ),
    c!(
        "Layout",
        "Card",
        [
            p("children", "Node[]"),
            o(
                "variant",
                "\"default\" | \"sunk\" | \"accent\" | \"success\" | \"warning\" | \"danger\""
            )
        ],
        "A bordered panel. The usual root."
    ),
    c!(
        "Layout",
        "CardHeader",
        [p("title", "string"), o("subtitle", "string")],
        "The card's title (the first one names the artifact) and an optional muted line."
    ),
    c!(
        "Layout",
        "InlineHeader",
        [p("title", "string"), o("description", "string")],
        "A section label inside a card."
    ),
    c!("Layout", "Separator", [], "A thin rule."),
    c!(
        "Layout",
        "Tabs",
        [p("items", "TabItem[]")],
        "Switchable panes."
    ),
    c!(
        "Layout",
        "TabItem",
        [
            p("value", "string"),
            p("trigger", "string"),
            p("content", "Node | Node[]")
        ],
        "One tab: an id, its label, what it shows."
    ),
    c!(
        "Layout",
        "Accordion",
        [p("items", "AccordionItem[]")],
        "Collapsible sections."
    ),
    c!(
        "Layout",
        "AccordionItem",
        [
            p("value", "string"),
            p("trigger", "string"),
            p("content", "Node | Node[]"),
            o("open", "boolean")
        ],
        "One collapsible section."
    ),
    c!(
        "Layout",
        "Steps",
        [p("items", "StepsItem[]")],
        "A numbered sequence: plans, walkthroughs, timelines."
    ),
    c!(
        "Layout",
        "StepsItem",
        [
            p("title", "string"),
            o("details", "string | Node | Node[]"),
            o("status", "\"done\" | \"active\" | \"todo\" | \"blocked\"")
        ],
        "One step, with a status pill when given."
    ),
    // ---------------------------------------------------------------- content
    c!(
        "Content",
        "TextContent",
        [
            p("text", "string"),
            o(
                "size",
                "\"small\" | \"default\" | \"large\" | \"large-heavy\""
            )
        ],
        "A paragraph."
    ),
    c!(
        "Content",
        "Markdown",
        [p("text", "string")],
        "Longer prose: paragraphs, - lists, 1. lists, ### headings, > quotes, fenced code."
    ),
    c!(
        "Content",
        "Callout",
        [
            p(
                "variant",
                "\"info\" | \"success\" | \"warning\" | \"danger\" | \"neutral\""
            ),
            p("title", "string"),
            o("description", "string")
        ],
        "A highlighted note: the root cause, a warning, the recommendation."
    ),
    c!(
        "Content",
        "CodeBlock",
        [
            p("language", "string"),
            p("code", "string"),
            o("filename", "string"),
            o("startLine", "number")
        ],
        "Code with line numbers (from startLine, default 1); it wraps on phones."
    ),
    c!(
        "Content",
        "Diff",
        [
            p("file", "string"),
            p("patch", "string"),
            o("note", "string")
        ],
        "A unified diff (@@ hunks, +/- lines) with old/new line numbers, green and red rows, and a one-line note on why."
    ),
    c!(
        "Content",
        "Table",
        [p("columns", "Col[]"), o("caption", "string")],
        "Rows from columns of equal length; becomes stacked cards on phones."
    ),
    c!(
        "Content",
        "Col",
        [
            p("label", "string"),
            p("data", "any[]"),
            o("type", "\"text\" | \"number\" | \"code\" | \"tag\"")
        ],
        "One table column. number aligns right; code is monospace; tag shows a pill."
    ),
    c!(
        "Content",
        "Metric",
        [
            p("value", "string | number"),
            p("label", "string"),
            o("delta", "string"),
            o("tone", "\"good\" | \"bad\" | \"neutral\"")
        ],
        "A big figure with its label and an optional change pill. Put several in a Grid."
    ),
    c!(
        "Content",
        "Progress",
        [
            p("value", "number"),
            o("max", "number"),
            o("label", "string"),
            o("tone", "\"accent\" | \"good\" | \"bad\" | \"warning\"")
        ],
        "A labelled bar, value out of max (default 100)."
    ),
    c!(
        "Content",
        "Tag",
        [
            p("text", "string"),
            o(
                "variant",
                "\"neutral\" | \"accent\" | \"success\" | \"warning\" | \"danger\" | \"violet\""
            )
        ],
        "A small pill."
    ),
    c!(
        "Content",
        "TagBlock",
        [p("tags", "(string | Tag)[]")],
        "A row of pills."
    ),
    c!(
        "Content",
        "ListBlock",
        [
            p("items", "(string | ListItem)[]"),
            o("variant", "\"bullet\" | \"number\" | \"plain\"")
        ],
        "A list."
    ),
    c!(
        "Content",
        "ListItem",
        [
            p("title", "string"),
            o("subtitle", "string"),
            o("meta", "string"),
            o("action", "Action")
        ],
        "A list row: title, a muted line, a right-aligned meta (a path, a count); tappable with an action."
    ),
    c!(
        "Content",
        "Tree",
        [p("items", "TreeItem[]")],
        "An expandable tree: files, modules, call graphs."
    ),
    c!(
        "Content",
        "TreeItem",
        [
            p("label", "string"),
            o("children", "TreeItem[]"),
            o("detail", "string | Node"),
            o("open", "boolean")
        ],
        "A tree node; tapping it toggles its children and shows its detail."
    ),
    c!(
        "Content",
        "Flow",
        [
            p("nodes", "FlowNode[]"),
            o("direction", "\"row\" | \"column\"")
        ],
        "A diagram of steps joined by arrows (a pipeline, a request path, a causal chain). Rows turn into columns on phones."
    ),
    c!(
        "Content",
        "FlowNode",
        [
            p("label", "string"),
            o("detail", "string"),
            o(
                "tone",
                "\"neutral\" | \"accent\" | \"success\" | \"warning\" | \"danger\" | \"violet\""
            )
        ],
        "One box in a Flow; detail is a short muted line (a file or function)."
    ),
    c!(
        "Content",
        "Image",
        [p("alt", "string"), p("src", "string")],
        "An https image."
    ),
    // ---------------------------------------------------------------- charts
    c!(
        "Charts",
        "BarChart",
        [
            p("labels", "string[]"),
            p("series", "Series[]"),
            o("variant", "\"grouped\" | \"stacked\""),
            o("xLabel", "string"),
            o("yLabel", "string")
        ],
        "Vertical bars; tap a bar for its value."
    ),
    c!(
        "Charts",
        "HorizontalBarChart",
        [
            p("labels", "string[]"),
            p("series", "Series[]"),
            o("variant", "\"grouped\" | \"stacked\"")
        ],
        "One bar per row: best for many or long labels, and on phones."
    ),
    c!(
        "Charts",
        "LineChart",
        [
            p("labels", "string[]"),
            p("series", "Series[]"),
            o("xLabel", "string"),
            o("yLabel", "string")
        ],
        "Lines over the labels."
    ),
    c!(
        "Charts",
        "AreaChart",
        [
            p("labels", "string[]"),
            p("series", "Series[]"),
            o("xLabel", "string"),
            o("yLabel", "string")
        ],
        "A line chart, filled."
    ),
    c!(
        "Charts",
        "Series",
        [p("name", "string"), p("values", "number[]")],
        "One data series."
    ),
    c!(
        "Charts",
        "PieChart",
        [
            p("labels", "string[]"),
            p("values", "number[]"),
            o("variant", "\"pie\" | \"donut\"")
        ],
        "Shares of a whole, labelled directly."
    ),
    // ---------------------------------------------------------------- interactive
    c!(
        "Interactive",
        "Button",
        [
            p("label", "string"),
            o("action", "Action | string"),
            o(
                "variant",
                "\"primary\" | \"secondary\" | \"ghost\" | \"danger\""
            )
        ],
        "A button. A plain string action is sent to you as the user's message."
    ),
    c!(
        "Interactive",
        "Buttons",
        [
            p("items", "Button[]"),
            o("direction", "\"row\" | \"column\"")
        ],
        "A group of buttons."
    ),
    c!(
        "Interactive",
        "FollowUpBlock",
        [p("items", "FollowUpItem[]")],
        "Suggested next questions, at the end."
    ),
    c!(
        "Interactive",
        "FollowUpItem",
        [p("text", "string")],
        "A suggestion; tapping it sends it to you."
    ),
    c!(
        "Interactive",
        "Form",
        [
            p("name", "string"),
            p("buttons", "Buttons"),
            p("fields", "FormControl[]")
        ],
        "Fields with buttons; an @ToAssistant from its buttons carries the field values."
    ),
    c!(
        "Interactive",
        "FormControl",
        [
            p("label", "string"),
            p("control", "Input | TextArea | Select | Switch | Slider"),
            o("hint", "string")
        ],
        "A labelled field."
    ),
    c!(
        "Interactive",
        "Input",
        [
            p("name", "string"),
            o("placeholder", "string"),
            o("type", "\"text\" | \"number\" | \"email\" | \"search\""),
            o(
                "rules",
                "{required?, email?, numeric?, min?, max?, minLength?, maxLength?}"
            ),
            o("value", "$binding")
        ],
        "A text field. Bound to a $variable it reads and writes it; else it writes $<name>."
    ),
    c!(
        "Interactive",
        "TextArea",
        [
            p("name", "string"),
            o("placeholder", "string"),
            o("rows", "number"),
            o("rules", "object"),
            o("value", "$binding")
        ],
        "A multi-line field."
    ),
    c!(
        "Interactive",
        "Select",
        [
            p("name", "string"),
            p("items", "SelectItem[]"),
            o("placeholder", "string"),
            o("rules", "object"),
            o("value", "$binding")
        ],
        "A dropdown."
    ),
    c!(
        "Interactive",
        "SelectItem",
        [p("value", "string"), p("label", "string")],
        "One choice."
    ),
    c!(
        "Interactive",
        "SegmentedControl",
        [
            p("name", "string"),
            p("items", "SelectItem[]"),
            o("value", "$binding")
        ],
        "Buttons that pick one choice, for filtering or switching what a $variable shows."
    ),
    c!(
        "Interactive",
        "Switch",
        [
            p("name", "string"),
            p("label", "string"),
            o("value", "$binding")
        ],
        "An on/off toggle."
    ),
    c!(
        "Interactive",
        "Slider",
        [
            p("name", "string"),
            p("min", "number"),
            p("max", "number"),
            o("step", "number"),
            o("value", "$binding"),
            o("label", "string")
        ],
        "A range slider."
    ),
    // ---------------------------------------------------------------- lessons
    c!(
        "Lessons",
        "Lesson",
        [p("goal", "string"), p("parts", "LessonPart[]")],
        "A lesson: what the reader will understand by the end, then its parts, from the simplest case up to that goal."
    ),
    c!(
        "Lessons",
        "LessonPart",
        [
            p("title", "string"),
            p("level", "string"),
            p("content", "Node[]"),
            o("checks", "(Check | QuickCheck)[]")
        ],
        "One step up: a short title, where it sits (a few words, e.g. \"The simplest case\", \"Adding retries\", \"The change\"), the explanation drawn with any components, and the questions that check it."
    ),
    c!(
        "Lessons",
        "Check",
        [
            p("question", "string"),
            o("hint", "string"),
            o("keyPoints", "string")
        ],
        "An open question the reader answers in their own words; their answer comes to you for feedback. The hint nudges without giving it away; keyPoints (what a good answer says) are shown once they've answered."
    ),
    c!(
        "Lessons",
        "QuickCheck",
        [
            p("question", "string"),
            p("options", "string[]"),
            p("answer", "number"),
            o("why", "string")
        ],
        "A multiple-choice question answered on the spot: answer is the right option's index from 0, why explains it (shown after any pick)."
    ),
];

pub fn component(name: &str) -> Option<&'static Component> {
    LIBRARY.iter().find(|c| c.name == name)
}

pub fn param_index(component_name: &str, param: &str) -> Option<usize> {
    component(component_name)?
        .params
        .iter()
        .position(|p| p.name == param)
}

impl Component {
    /// `Name(param: type, param?: type)`.
    pub fn signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| format!("{}{}: {}", p.name, if p.required { "" } else { "?" }, p.ty))
            .collect();
        format!("{}({})", self.name, params.join(", "))
    }
}

/// The library as the prompt lists it: each group, then one line a component.
pub fn reference() -> String {
    let mut out = String::new();
    for (group, about) in GROUPS {
        out.push_str(&format!("### {group}\n\n{about}\n\n"));
        for c in LIBRARY.iter().filter(|c| c.group == *group) {
            out.push_str(&format!("- `{}`: {}\n", c.signature(), c.doc));
        }
        out.push('\n');
    }
    out
}

/// Arguments that don't fit their component's signature, for the page to point out.
pub fn check(node: &crate::Node) -> Vec<String> {
    let Some(c) = component(&node.name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if node.args.len() > c.params.len() {
        out.push(format!(
            "{} takes {} arguments, not {}",
            c.name,
            c.params.len(),
            node.args.len()
        ));
    }
    for (i, p) in c.params.iter().enumerate() {
        if p.required && node.args.get(i).is_none_or(crate::Value::is_null) {
            out.push(format!("{} is missing {}", c.name, p.name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_the_reference_lists_them_all() {
        let r = reference();
        for (i, c) in LIBRARY.iter().enumerate() {
            assert!(
                LIBRARY[..i].iter().all(|d| d.name != c.name),
                "{} twice",
                c.name
            );
            assert!(GROUPS.iter().any(|(g, _)| *g == c.group));
            assert!(r.contains(&format!("`{}(", c.name)), "{}", c.name);
        }
        assert!(r.contains("`CardHeader(title: string, subtitle?: string)`"));
    }
}
