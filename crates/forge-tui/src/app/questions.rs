//! Inline `ask_user_question` prompt for [`TuiApp`].

use super::*;
use crate::conversation::{QuestionMenuRow, QuestionPendingPresentation};
use forge_types::{
    AskUserQuestionAnswerItem, AskUserQuestionItem, AskUserQuestionResult, QuestionPayload,
};

pub(crate) const OTHER_LABEL: &str = "Other";

#[derive(Debug, Clone, Default)]
struct QuestionMenuState {
    call_id: Option<String>,
    question_idx: usize,
    option_idx: usize,
    chosen: Vec<Vec<String>>,
    custom: Vec<Option<String>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct QuestionSessionState {
    menu: QuestionMenuState,
}

#[derive(Debug, Clone)]
pub(crate) enum QuestionSubmit {
    Answers(AskUserQuestionResult),
    Dismiss,
}

impl TuiApp {
    pub(super) fn question_menu_indexes(&self) -> (usize, usize) {
        (
            self.question_session.menu.question_idx,
            self.question_session.menu.option_idx,
        )
    }

    pub(super) fn sync_question_menu(&mut self) {
        match self.session.pending_question() {
            None => self.question_session.menu = QuestionMenuState::default(),
            Some(payload) => {
                if self.question_session.menu.call_id.as_deref() != Some(payload.call_id.as_str()) {
                    self.question_session.menu = QuestionMenuState {
                        call_id: Some(payload.call_id.clone()),
                        question_idx: 0,
                        option_idx: 0,
                        chosen: vec![Vec::new(); payload.questions.len()],
                        custom: vec![None; payload.questions.len()],
                    };
                }
                let n = self.question_row_count(payload);
                if n > 0 {
                    self.question_session.menu.option_idx =
                        self.question_session.menu.option_idx.min(n - 1);
                }
                if !payload.questions.is_empty() {
                    self.question_session.menu.question_idx = self
                        .question_session
                        .menu
                        .question_idx
                        .min(payload.questions.len() - 1);
                }
            }
        }
    }

    pub(super) fn sync_question_focus(&mut self) {
        let Some(payload) = self.session.pending_question() else {
            return;
        };
        if self.question_session.menu.call_id.as_deref() != Some(payload.call_id.as_str()) {
            self.focus_block(FocusBlock::Approval);
            self.conversation_view.follow = true;
        }
    }

    fn current_question<'a>(
        &self,
        payload: &'a QuestionPayload,
    ) -> Option<&'a AskUserQuestionItem> {
        payload
            .questions
            .get(self.question_session.menu.question_idx)
    }

    fn question_row_count(&self, payload: &QuestionPayload) -> usize {
        let Some(question) = self.current_question(payload) else {
            return 0;
        };
        question.options.len() + 1
    }

    pub(super) fn question_presentation(&self) -> Option<QuestionPendingPresentation> {
        let payload = self.session.pending_question()?;
        let idx = self.question_session.menu.question_idx;
        let question = payload.questions.get(idx)?;
        let chosen = self
            .question_session
            .menu
            .chosen
            .get(idx)
            .cloned()
            .unwrap_or_default();
        let custom = self
            .question_session
            .menu
            .custom
            .get(idx)
            .cloned()
            .flatten();
        let mut options: Vec<QuestionMenuRow> = question
            .options
            .iter()
            .map(|option| QuestionMenuRow {
                label: option.label.clone(),
                description: (!option.description.is_empty()).then(|| option.description.clone()),
                chosen: chosen.iter().any(|label| label == &option.label),
            })
            .collect();
        let other_label = match custom.as_deref() {
            Some(text) if !text.is_empty() => format!("{OTHER_LABEL}: {text}"),
            _ => OTHER_LABEL.to_string(),
        };
        options.push(QuestionMenuRow {
            label: other_label,
            description: Some("Type a custom answer in the composer.".into()),
            chosen: custom.is_some(),
        });
        Some(QuestionPendingPresentation {
            header: question.header.clone(),
            question: question.question.clone(),
            options,
            selected: self.question_session.menu.option_idx,
            multi_select: question.multi_select,
            question_index: idx,
            question_count: payload.questions.len(),
            focused: self.focus.block() == FocusBlock::Approval,
        })
    }

    pub(super) async fn handle_question_menu_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if self.session.pending_question().is_none() {
            return Ok(false);
        }
        if self.focus.block() != FocusBlock::Approval {
            return Ok(false);
        }
        self.sync_question_menu();
        let Some(payload) = self.session.pending_question().cloned() else {
            return Ok(false);
        };
        let rows = self.question_row_count(&payload).max(1);
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                self.question_session.menu.option_idx =
                    (self.question_session.menu.option_idx + rows - 1) % rows;
                Ok(true)
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.question_session.menu.option_idx =
                    (self.question_session.menu.option_idx + 1) % rows;
                Ok(true)
            }
            KeyCode::Left | KeyCode::Char('h') if key.modifiers.is_empty() => {
                if payload.questions.len() > 1 {
                    let n = payload.questions.len();
                    self.question_session.menu.question_idx =
                        (self.question_session.menu.question_idx + n - 1) % n;
                    self.question_session.menu.option_idx = 0;
                }
                Ok(true)
            }
            KeyCode::Right | KeyCode::Char('l') if key.modifiers.is_empty() => {
                if payload.questions.len() > 1 {
                    let n = payload.questions.len();
                    self.question_session.menu.question_idx =
                        (self.question_session.menu.question_idx + 1) % n;
                    self.question_session.menu.option_idx = 0;
                }
                Ok(true)
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                self.toggle_current_option(&payload);
                Ok(true)
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.queue_question_submit(QuestionSubmit::Dismiss);
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.confirm_current_option(&payload);
                Ok(true)
            }
            KeyCode::Char(c) if key.modifiers.is_empty() && c.is_ascii_digit() => {
                let digit = c.to_digit(10).unwrap_or(0) as usize;
                if digit >= 1
                    && digit
                        <= payload
                            .questions
                            .get(self.question_session.menu.question_idx)
                            .map(|q| q.options.len())
                            .unwrap_or(0)
                {
                    self.question_session.menu.option_idx = digit - 1;
                    self.confirm_current_option(&payload);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn toggle_current_option(&mut self, payload: &QuestionPayload) {
        let q_idx = self.question_session.menu.question_idx;
        let Some(question) = payload.questions.get(q_idx) else {
            return;
        };
        if !question.multi_select {
            return;
        }
        let opt_idx = self.question_session.menu.option_idx;
        if opt_idx >= question.options.len() {
            return;
        }
        let label = question.options[opt_idx].label.clone();
        let chosen = &mut self.question_session.menu.chosen[q_idx];
        if let Some(pos) = chosen.iter().position(|item| item == &label) {
            chosen.remove(pos);
        } else {
            chosen.push(label);
        }
    }

    fn confirm_current_option(&mut self, payload: &QuestionPayload) {
        let q_idx = self.question_session.menu.question_idx;
        let Some(question) = payload.questions.get(q_idx) else {
            return;
        };
        let opt_idx = self.question_session.menu.option_idx;
        if opt_idx >= question.options.len() {
            if self
                .question_session
                .menu
                .custom
                .get(q_idx)
                .and_then(|c| c.as_ref())
                .is_none()
            {
                self.enter_chat_composer();
                self.set_feedback(
                    FeedbackSeverity::Info,
                    "type a custom answer and press Enter",
                );
                return;
            }
        } else if question.multi_select {
            self.toggle_current_option(payload);
            if self.question_session.menu.chosen[q_idx].is_empty()
                && self.question_session.menu.custom[q_idx].is_none()
            {
                return;
            }
        } else {
            self.question_session.menu.chosen[q_idx] =
                vec![question.options[opt_idx].label.clone()];
            self.question_session.menu.custom[q_idx] = None;
        }
        if q_idx + 1 < payload.questions.len() {
            self.question_session.menu.question_idx = q_idx + 1;
            self.question_session.menu.option_idx = 0;
            return;
        }
        if let Some(result) = self.collect_question_result(payload) {
            self.queue_question_submit(QuestionSubmit::Answers(result));
        }
    }

    fn collect_question_result(&self, payload: &QuestionPayload) -> Option<AskUserQuestionResult> {
        let mut answers = Vec::with_capacity(payload.questions.len());
        for (idx, question) in payload.questions.iter().enumerate() {
            let selected = self
                .question_session
                .menu
                .chosen
                .get(idx)
                .cloned()
                .unwrap_or_default();
            let custom = self
                .question_session
                .menu
                .custom
                .get(idx)
                .cloned()
                .flatten();
            if selected.is_empty() && custom.is_none() {
                return None;
            }
            answers.push(AskUserQuestionAnswerItem {
                id: question.id.clone(),
                selected,
                custom,
            });
        }
        Some(AskUserQuestionResult { answers })
    }

    pub(super) fn apply_clarification_text(&mut self, text: &str) {
        let Some(payload) = self.session.pending_question().cloned() else {
            return;
        };
        self.sync_question_menu();
        let q_idx = self.question_session.menu.question_idx;
        if q_idx >= payload.questions.len() {
            return;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(custom) = self.question_session.menu.custom.get_mut(q_idx) {
            *custom = Some(trimmed.to_string());
        }
        if !payload.questions[q_idx].multi_select {
            if let Some(chosen) = self.question_session.menu.chosen.get_mut(q_idx) {
                chosen.clear();
            }
        }
        if q_idx + 1 < payload.questions.len() {
            self.question_session.menu.question_idx = q_idx + 1;
            self.question_session.menu.option_idx = 0;
            self.focus_block(FocusBlock::Approval);
            return;
        }
        if let Some(result) = self.collect_question_result(&payload) {
            self.queue_question_submit(QuestionSubmit::Answers(result));
        }
    }

    fn queue_question_submit(&mut self, submit: QuestionSubmit) {
        self.pending_interaction.request_question_submit(submit);
        if let Some(payload) = self.session.pending_question() {
            self.busy_state.start(BusyPhase::Tool {
                name: payload.tool.clone(),
            });
        }
    }

    pub async fn drain_pending_question(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let Some(submit) = self.pending_interaction.take_question_submit() else {
            return Ok(());
        };
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let answers = match submit {
            QuestionSubmit::Answers(result) => Some(result),
            QuestionSubmit::Dismiss => None,
        };
        let dismissed = answers.is_none();
        // Same ownership rule as approvals: a sibling's question was asked by
        // a supervisor-owned actor, so the answers go back through it.
        if let SelectedRuntime::Sibling(session_id) = self.selected_runtime() {
            if self
                .selected_snapshot()
                .is_some_and(|snapshot| snapshot.session.pending_question.is_some())
            {
                self.send_task_command(forge_session::SupervisorCommand::ResolveQuestion {
                    session_id,
                    answers,
                    actor: "tui".into(),
                })
                .await;
                self.status_state.message = if dismissed {
                    "Questions skipped".into()
                } else {
                    "Question answered".into()
                };
                self.push_toast(self.status_state.message.clone());
                if let Some(term) = terminal {
                    let _ = term.draw(|f| self.draw(f));
                }
                return Ok(());
            }
        }
        self.session.resolve_question(answers, "tui").await?;
        self.status_state.message = if dismissed {
            "Questions skipped".into()
        } else {
            "Question answered".into()
        };
        self.push_toast(self.status_state.message.clone());
        self.resume_turn_after_hitl();
        self.enter_chat_composer();
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }
}
