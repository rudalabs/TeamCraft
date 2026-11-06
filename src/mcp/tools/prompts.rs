//! The MCP prompts (`prompts/list`, `prompts/get`).

use super::*;

pub(super) struct Prompt {
    pub(super) name: &'static str,
    title: &'static str,
    description: &'static str,
    /// (name, description, required)
    arguments: &'static [(&'static str, &'static str, bool)],
}

pub(super) const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "release_notes",
        title: "Release notes",
        description: "Release notes for a revision: what changed compared with the previous one.",
        arguments: &[
            ("item", "Item number, e.g. ASM-000003", true),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
    Prompt {
        name: "review_change",
        title: "Review a change order",
        description: "Prepares the review of a change order: scope, risks, open questions.",
        arguments: &[("change", "Change number, e.g. ECO-000004", true)],
    },
    Prompt {
        name: "release_check",
        title: "Release check",
        description: "Checks whether a revision and its structure are ready to be released.",
        arguments: &[
            ("item", "Item number", true),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
    Prompt {
        name: "worklist_triage",
        title: "Worklist triage",
        description: "Orders what is waiting for me into a short to-do list.",
        arguments: &[],
    },
    Prompt {
        name: "triage_requests",
        title: "Prepare request triage",
        description:
            "Summarises the open change requests assigned to me (and new unassigned ones) so I \
            can triage them; the decisions stay mine.",
        arguments: &[],
    },
    Prompt {
        name: "simulation_status",
        title: "Simulation status",
        description:
            "Whether the simulations of a part or assembly – or one simulation – still match \
            their bases, and how the results look.",
        arguments: &[
            (
                "item",
                "Part, assembly or simulation number, e.g. SIM-000003",
                true,
            ),
            ("revision", "Revision code (default: latest)", false),
        ],
    },
];

/// `prompts/list`.
pub fn prompts() -> Vec<Value> {
    PROMPTS
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "title": p.title,
                "description": p.description,
                "arguments": p.arguments.iter().map(|(name, description, required)| json!({
                    "name": name, "description": description, "required": required,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// `prompts/get`: the prompt as one user message.
pub fn prompt(name: &str, args: &Value) -> Result<Value, String> {
    let p = PROMPTS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("unknown prompt `{name}`"))?;
    let arg = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    for (key, _, required) in p.arguments {
        if *required && arg(key).is_none() {
            return Err(format!("prompt `{name}` needs the argument `{key}`"));
        }
    }
    let revision = arg("revision").unwrap_or_else(|| "latest".into());
    let text = match name {
        "release_notes" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Write release notes for {item} revision {revision} in TeamCraft.\n\
                 1. Call compare_revisions with item={item} and to={revision} (it compares with the revision before).\n\
                 2. Call get_revision for {item}/{revision} for the description and the change orders it belongs to; \
                 if there is one, call get_change for its title and reason.\n\
                 Produce: a one-line summary; then short sections Attributes, Files (added, removed, new content) and BOM \
                 (added, removed, quantity changes), leaving out empty sections; then Open points (e.g. unreleased children, \
                 missing mass). Quote item numbers and revisions exactly. Do not invent reasons the data does not give."
            )
        }
        "review_change" => {
            let change = arg("change").unwrap_or_default();
            format!(
                "Help me review change order {change} in TeamCraft.\n\
                 1. Call get_change with change={change}.\n\
                 2. For each revision in it, call compare_revisions (item, to=its revision) to see what it changes; \
                 for assemblies also call bom_health with rule=released.\n\
                 3. Call suggest_reviewers with change={change} to see who else knows the affected items.\n\
                 Produce a concise review for an engineer: Scope (what is released, in one line each); Risks (readiness \
                 blockers and warnings, dependents from the impact analysis that are not in the change, check-outs, \
                 missing files or mass); Questions for the author; people who should also look at it (only from \
                 suggest_reviewers, with its reason; it never assigns them); a suggestion whether it looks ready. The decision \
                 (approve or reject) is mine – you cannot and should not make it."
            )
        }
        "release_check" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Check whether {item} revision {revision} is ready to be released.\n\
                 1. Call get_revision for {item}/{revision}.\n\
                 2. Call bom_health with item={item}, revision={revision}, rule=released.\n\
                 3. Call find_duplicate_files with item={item}, revision={revision}.\n\
                 4. If it is a new part (revision A), call find_similar_parts with item={item}, revision={revision} for \
                 existing parts of nearly the same shape (score 90 or more).\n\
                 Produce a checklist: Blocking (must be fixed or released together: unreleased or unresolved BOM lines and \
                 references, check-outs) and Warnings (missing mass or files, obsolete parts, duplicate files, near-duplicate \
                 existing parts, open \
                 checks). End with the items a change order would need. Offer to draft it with create_change_draft, but \
                 only create it if I say so."
            )
        }
        "worklist_triage" => "Triage my TeamCraft worklist.\n\
             Call my_worklist. Produce a short ordered to-do list: reviews waiting for my decision first (oldest \
             first), then open impacts to check, then check-outs that have been open long (remind me to check in or \
             cancel), then my own change orders that are drafts or were rejected. One line per entry with the item or \
             change number and the next action. Say plainly if there is nothing to do."
            .to_string(),
        "triage_requests" => "Prepare the triage of my TeamCraft change requests.\n\
             1. Call list_change_requests with scope=assigned and status=open.\n\
             2. Call list_change_requests with status=new for requests nobody has looked at yet.\n\
             3. For each request found (at most 10, highest priority and oldest due date first), call \
             get_change_request; for an affected item that looks central, call get_item for its state, open \
             change orders and simulations.\n\
             Produce one short block per request: number and title; the problem in one sentence; affected items \
             with their latest state; what the discussion already settled and what is open; duplicates or \
             related requests; a suggested next step (ask the requester, check a simulation, candidate for \
             accepting, deferring or rejecting – with the reason). You cannot triage, accept, defer or reject: \
             those decisions are mine and are made in TeamCraft. Say plainly if there is nothing to triage."
            .to_string(),
        "simulation_status" => {
            let item = arg("item").unwrap_or_default();
            format!(
                "Report the simulation status of {item} revision {revision} in TeamCraft.\n\
                 1. Call get_item with item={item}. If it is a simulation, call get_simulation with item={item} \
                 and revision={revision}, and compare_simulation_results with item={item} and to={revision} when \
                 there is an earlier revision.\n\
                 2. Otherwise call list_simulations with item={item}, then get_simulation for each simulation \
                 that is out of date or has failed results.\n\
                 Produce: a table of the simulations (number, revision, state, up to date or the out-of-date \
                 reason, results passed/failed); then the failed values with value, limit and load case; then \
                 what changed in the results between the revisions, if compared; then a recommendation which \
                 simulations need to be re-run or re-checked. Do not invent values the data does not give."
            )
        }
        _ => unreachable!("prompt names are checked above"),
    };
    Ok(json!({
        "description": p.description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}
