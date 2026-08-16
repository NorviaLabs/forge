//! The compaction instruction appended to the live model-visible context.
//!
//! Cache-friendliness is the whole point of the shape here (§7): the request
//! Forge sends to produce a checkpoint is the *exact* current context with one
//! extra user message on the end. Everything before it is byte-identical to
//! the previous request, so the provider's cached prefix is reused for the
//! compaction call itself instead of paying full price for a second copy of
//! the conversation.

use forge_types::{Message, MessageRole};

use super::checkpoint::{Checkpoint, CHECKPOINT_ROOT, CHECKPOINT_VERSION};
use super::facts::{recent_facts, ProtectedFact};

const BASE_INSTRUCTION: &str = r#"[forge:compaction] This is an internal Forge maintenance request, not a message from the user. Do not call any tools. Do not answer the task. Reply with the checkpoint element and nothing else.

Create a compact state checkpoint that another coding agent could use to continue this task without access to any earlier conversation history.

Record current state, not a chronological narrative. Prioritise, in order:
1. explicit user constraints
2. user corrections
3. explicit user decisions
4. the current objective
5. current implementation state
6. important technical decisions
7. relevant files and symbols
8. failed approaches and pitfalls
9. pending work
10. the immediate next action

De-prioritise and omit: verbose tool output, exploratory reasoning that can be reproduced by re-reading the repository, repeated discussion, superseded plans, and anything already contradicted later in the conversation.

An explicit user instruction is the highest-value thing in this conversation: repository state can be rediscovered by reading files, but a constraint the user stated exists nowhere else. Losing one is a correctness failure. Quote such instructions closely rather than paraphrasing them away."#;

const MERGE_INSTRUCTION: &str = r#"A previous checkpoint is already present in this context. Treat it as existing structured state, not as conversation to be summarised. For every fact in it: preserve it if still valid, update it if superseded, remove it if obsolete, and add anything newly established since. Do not narratively summarise the previous checkpoint. Merge it with the newer conversation into one current-state checkpoint that describes the present truth."#;

const FORMAT_INSTRUCTION: &str = r#"Reply with exactly this element and nothing outside it. Omit a section only when it genuinely has no content.

<forge_checkpoint version="1">

<objective>
What the user currently wants accomplished.
</objective>

<user_constraints>
Explicit requirements, prohibitions, preferences and corrections stated by the user.
</user_constraints>

<decisions>
Technical and product decisions that remain valid.
</decisions>

<completed>
Work already finished that matters for continuing.
</completed>

<current_work>
What was actively in progress immediately before this checkpoint.
</current_work>

<files>
Relevant files and why they matter.
</files>

<symbols>
Important modules, functions, types, identifiers and APIs.
</symbols>

<commands_and_results>
Important commands and their outcomes only.
</commands_and_results>

<failures>
Failed approaches, known issues, and things not to repeat.
</failures>

<pending>
Outstanding tasks.
</pending>

<next_action>
The most likely immediate next step.
</next_action>

</forge_checkpoint>"#;

/// Build the instruction text appended to the live context.
pub fn compaction_instruction(
    previous_checkpoint: Option<&Checkpoint>,
    protected_facts: &[ProtectedFact],
) -> String {
    let mut prompt = String::with_capacity(4096);
    prompt.push_str(BASE_INSTRUCTION);

    if previous_checkpoint.is_some() {
        prompt.push_str("\n\n");
        prompt.push_str(MERGE_INSTRUCTION);
    }

    let facts = recent_facts(protected_facts);
    if !facts.is_empty() {
        prompt.push_str(
            "\n\nThese user-authored statements were taken verbatim from this session's history. \
             Every one that is still in force must be represented in <user_constraints> \
             (or, where it is now settled, in <decisions>):\n",
        );
        for fact in facts {
            prompt.push_str(&format!(
                "\n- ({}) {}",
                fact.kind.as_str(),
                one_line(&fact.text)
            ));
        }
    }

    prompt.push_str("\n\n");
    prompt.push_str(FORMAT_INSTRUCTION);
    debug_assert!(prompt.contains(CHECKPOINT_ROOT));
    debug_assert!(prompt.contains(&CHECKPOINT_VERSION.to_string()));
    prompt
}

/// The instruction as an appended user message — the only difference between
/// a compaction request and the normal next request.
pub fn compaction_message(
    previous_checkpoint: Option<&Checkpoint>,
    protected_facts: &[ProtectedFact],
) -> Message {
    Message::new(
        MessageRole::User,
        compaction_instruction(previous_checkpoint, protected_facts),
    )
}

/// The checkpoint as it appears in model-visible context after installation.
///
/// A system message so providers that hoist system content (Anthropic) fold
/// it into the cacheable prefix alongside the base system prompt, which is
/// exactly where the new epoch's stable prefix should end.
pub fn checkpoint_message(checkpoint: &Checkpoint) -> Message {
    Message::new(
        MessageRole::System,
        format!(
            "# Context Checkpoint\n\nEarlier conversation in this session has been compacted. \
             The structured state below replaces it and is authoritative; the messages after it \
             are the most recent raw conversation, kept verbatim.\n\n{}",
            checkpoint.render()
        ),
    )
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::facts::{ProtectedFact, ProtectedFactKind};

    fn fact(text: &str) -> ProtectedFact {
        ProtectedFact {
            kind: ProtectedFactKind::UserConstraint,
            text: text.into(),
            source_message_index: 0,
        }
    }

    #[test]
    fn first_compaction_has_no_merge_clause() {
        let prompt = compaction_instruction(None, &[]);
        assert!(prompt.contains("compact state checkpoint"));
        assert!(!prompt.contains("previous checkpoint is already present"));
        assert!(prompt.contains("<forge_checkpoint version=\"1\">"));
    }

    #[test]
    fn repeated_compaction_asks_for_a_merge_not_a_summary_of_a_summary() {
        let previous = Checkpoint::parse(
            "<forge_checkpoint version=\"1\"><objective>o</objective><next_action>n</next_action></forge_checkpoint>",
        )
        .unwrap();
        let prompt = compaction_instruction(Some(&previous), &[]);
        assert!(prompt.contains("Do not narratively summarise the previous checkpoint"));
        assert!(prompt.contains("preserve it if still valid"));
    }

    #[test]
    fn protected_facts_are_listed_verbatim_and_flattened_to_one_line() {
        let prompt = compaction_instruction(None, &[fact("Do not change\nthe public API.")]);
        assert!(prompt.contains("- (user constraint) Do not change the public API."));
    }

    #[test]
    fn the_instruction_is_the_last_message_and_carries_the_internal_marker() {
        let message = compaction_message(None, &[]);
        assert_eq!(message.role, MessageRole::User);
        assert!(message.content.starts_with("[forge:compaction]"));
    }

    #[test]
    fn installed_checkpoint_is_a_system_message_that_reparses() {
        let checkpoint = Checkpoint::parse(
            "<forge_checkpoint version=\"1\"><objective>o</objective><next_action>n</next_action></forge_checkpoint>",
        )
        .unwrap();
        let message = checkpoint_message(&checkpoint);
        assert_eq!(message.role, MessageRole::System);
        assert_eq!(Checkpoint::parse(&message.content).unwrap(), checkpoint);
    }
}
