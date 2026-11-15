//! The Agents tab of a Conversation's sidebar: how its agent works with others, read
//! from its own tool calls. Claude Code's `Agent` (formerly `Task`) starts subagents,
//! `SendMessage` writes to them or to other sessions, and `ListAgents` reports the
//! sessions it can reach. Nothing here is stored; it is derived from the transcript.

use serde_json::Value;

use crate::{
    app::ConversationSession,
    harness::{ContentBlock, ToolActivity},
};

/// Tools through which an agent works with other agents.
const COORDINATION_TOOLS: [&str; 4] = ["Agent", "Task", "SendMessage", "ListAgents"];

/// How many coordination calls `session` holds; the Agents tab exists from the first.
pub fn coordination_calls(session: &ConversationSession) -> usize {
    session
        .tool_call_order()
        .iter()
        .filter(|(name, _)| COORDINATION_TOOLS.contains(&name.as_str()))
        .count()
}

/// When to open a Conversation's Agents tab so the user can follow along: once per
/// turn, at the first coordination call that arrives while the turn runs. Calls
/// already in the transcript when it is first seen, or loaded from history, never open
/// it, and what the user does with the sidebar afterwards holds for the rest of the turn.
#[derive(Clone, Copy, Debug)]
pub struct Follow {
    seen: usize,
    opened: bool,
}

impl Follow {
    pub fn new(count: usize) -> Self {
        Self {
            seen: count,
            opened: false,
        }
    }

    /// Record `count` coordination calls; true when the Agents tab should open now.
    pub fn observe(&mut self, count: usize, running: bool) -> bool {
        if !running {
            self.opened = false;
        }
        let open = running && count > self.seen && !self.opened;
        self.seen = count;
        self.opened |= open;
        open
    }
}

/// What a coordination call came to. Without a result, a call in a running turn is
/// still pending and any other call is unknown: a missing answer is not success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Done,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    /// A subagent started with `Agent` or `Task`.
    Spawn {
        description: Option<String>,
        agent_type: Option<String>,
        background: bool,
    },
    /// A `SendMessage` to a subagent, teammate or session.
    Message {
        to: String,
        text: String,
        notify_when_idle: bool,
    },
    /// A `ListAgents` call; its sessions are in [`Activity::sessions`] when it is the latest.
    Listing { sessions: Option<usize> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub call_id: String,
    pub kind: EventKind,
    pub outcome: Outcome,
    /// The first line of the call's result, as the harness reported it.
    pub result: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub name: String,
    /// The `[ref]` that disambiguates sessions with one name.
    pub reference: Option<String>,
    /// Kind, state and age as listed, in order; their wording is the harness's own.
    pub details: Vec<String>,
}

impl SessionRow {
    pub fn is_busy(&self) -> bool {
        self.details.iter().any(|detail| detail == "busy")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionGroup {
    /// The listing's own heading, e.g. `Peer sessions (4)`.
    pub title: String,
    pub rows: Vec<SessionRow>,
}

/// A `ListAgents` answer. Rows that do not follow the listed shape stay in `raw`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionList {
    /// The name other sessions use to reach this Conversation's agent.
    pub own: Option<String>,
    pub groups: Vec<SessionGroup>,
    pub raw: String,
}

impl SessionList {
    pub fn parse(text: &str) -> Self {
        let mut own = None;
        let mut groups: Vec<SessionGroup> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("This session is ") {
                own = Some(rest.split(" — ").next().unwrap_or(rest).trim().to_owned());
                continue;
            }
            if !line.starts_with(' ') && trimmed.ends_with(':') {
                groups.push(SessionGroup {
                    title: trimmed.trim_end_matches(':').to_owned(),
                    rows: Vec::new(),
                });
                continue;
            }
            let mut fields = trimmed.split('·').map(str::trim);
            let (Some(head), Some(group)) = (fields.next(), groups.last_mut()) else {
                continue;
            };
            let details: Vec<String> = fields
                .filter(|field| !field.is_empty())
                .map(str::to_owned)
                .collect();
            if details.is_empty() {
                continue;
            }
            let (name, reference) = match head.rsplit_once(" [") {
                Some((name, reference)) if reference.ends_with(']') => (
                    name.trim().to_owned(),
                    Some(reference.trim_end_matches(']').to_owned()),
                ),
                _ => (head.to_owned(), None),
            };
            group.rows.push(SessionRow {
                name,
                reference,
                details,
            });
        }
        groups.retain(|group| !group.rows.is_empty());
        Self {
            own,
            groups,
            raw: text.to_owned(),
        }
    }

    pub fn len(&self) -> usize {
        self.groups.iter().map(|group| group.rows.len()).sum()
    }
}

/// Coordination in one Conversation, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    pub events: Vec<Event>,
    /// The latest `ListAgents` answer that could be read.
    pub sessions: Option<SessionList>,
}

impl Activity {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.sessions.is_none()
    }

    pub fn messages_to(&self, name: &str) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::Message { to, .. } if to == name))
            .count()
    }

    pub fn from_session(session: &ConversationSession, turn_running: bool) -> Self {
        let mut activity = Self::default();
        for (name, call_id) in session.tool_call_order() {
            // Other tools never become events; skip them before joining their results.
            if !COORDINATION_TOOLS.contains(&name.as_str()) {
                continue;
            }
            let Some(ContentBlock::ToolCall { input, .. }) = session.tool_call_block(call_id)
            else {
                continue;
            };
            let (outcome, text) =
                result(session.tools.get(call_id), session, call_id, turn_running);
            let Some(event) = event(name, call_id, input, outcome, text.as_deref()) else {
                continue;
            };
            if let (EventKind::Listing { .. }, Outcome::Done, Some(text)) =
                (&event.kind, event.outcome, &text)
            {
                activity.sessions = Some(SessionList::parse(text));
            }
            activity.events.push(event);
        }
        activity
    }
}

/// What [`Activity::from_session`] reads, so an unchanged session reuses its activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivityKey {
    generation: uuid::Uuid,
    transcript_revision: u64,
    tools_revision: u64,
    turn_running: bool,
}

impl ActivityKey {
    fn of(session: &ConversationSession, turn_running: bool) -> Self {
        Self {
            generation: session.generation,
            transcript_revision: session.transcript_revision,
            tools_revision: session.tools_revision(),
            turn_running,
        }
    }
}

/// The Agents tab's activity, derived again only when the session it reads changed.
#[derive(Default)]
pub struct ActivityCache(Option<(ActivityKey, Activity)>);

impl ActivityCache {
    pub fn get(&mut self, session: &ConversationSession, turn_running: bool) -> &Activity {
        self.cached(ActivityKey::of(session, turn_running), || {
            Activity::from_session(session, turn_running)
        })
    }

    fn cached(&mut self, key: ActivityKey, build: impl FnOnce() -> Activity) -> &Activity {
        if self.0.as_ref().is_none_or(|(cached, _)| *cached != key) {
            self.0 = Some((key, build()));
        }
        &self.0.as_ref().expect("filled above").1
    }
}

/// One coordination event from a tool call, or `None` for any other tool.
fn event(
    name: &str,
    call_id: &str,
    input: &Value,
    outcome: Outcome,
    text: Option<&str>,
) -> Option<Event> {
    let string = |key: &str| {
        input[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let kind = match name {
        "Agent" | "Task" => EventKind::Spawn {
            description: string("description"),
            agent_type: string("subagent_type"),
            background: input["run_in_background"].as_bool().unwrap_or(false),
        },
        "SendMessage" => EventKind::Message {
            to: string("to").unwrap_or_default(),
            text: input["message"].as_str().unwrap_or_default().to_owned(),
            notify_when_idle: input["notify_when_idle"].as_bool().unwrap_or(false),
        },
        "ListAgents" => EventKind::Listing {
            sessions: text
                .filter(|_| outcome == Outcome::Done)
                .map(|text| SessionList::parse(text).len()),
        },
        _ => return None,
    };
    Some(Event {
        call_id: call_id.to_owned(),
        kind,
        outcome,
        result: text
            .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
            .map(str::to_owned),
    })
}

/// The settled outcome and joined text of a call: the live activity when complete,
/// else the recorded result message.
fn result(
    live: Option<&std::sync::Arc<ToolActivity>>,
    session: &ConversationSession,
    call_id: &str,
    turn_running: bool,
) -> (Outcome, Option<String>) {
    let text = |blocks: &[ContentBlock]| {
        let text: Vec<&str> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        (!text.is_empty()).then(|| text.join("\n"))
    };
    if let Some(tool) = live.filter(|tool| tool.complete) {
        let outcome = if tool.error {
            Outcome::Failed
        } else {
            Outcome::Done
        };
        return (outcome, text(&tool.result));
    }
    if let Some(message) = session.tool_result(call_id) {
        let outcome = if message.error {
            Outcome::Failed
        } else {
            Outcome::Done
        };
        return (outcome, text(&message.content));
    }
    let outcome = if turn_running {
        Outcome::Pending
    } else {
        Outcome::Unknown
    };
    (outcome, None)
}

#[cfg(test)]
mod tests {
    use super::{
        Activity, ActivityCache, ActivityKey, EventKind, Follow, Outcome, SessionList, event,
    };
    use serde_json::json;

    #[test]
    fn activity_is_derived_again_only_when_its_session_changed() {
        let key = ActivityKey {
            generation: uuid::Uuid::from_u128(1),
            transcript_revision: 4,
            tools_revision: 2,
            turn_running: true,
        };
        let mut cache = ActivityCache::default();
        let mut builds = 0;
        let mut get = |cache: &mut ActivityCache, key| {
            cache.cached(key, || {
                builds += 1;
                Activity::default()
            });
        };
        get(&mut cache, key);
        get(&mut cache, key);
        for changed in [
            ActivityKey {
                transcript_revision: 5,
                ..key
            },
            ActivityKey {
                tools_revision: 3,
                ..key
            },
            ActivityKey {
                turn_running: false,
                ..key
            },
            ActivityKey {
                generation: uuid::Uuid::from_u128(2),
                ..key
            },
        ] {
            get(&mut cache, changed);
        }
        assert_eq!(builds, 5, "one build, then one per changed input");
    }

    /// A `ListAgents` answer in the shape Claude Code returns it.
    const LISTING: &str = "This session is drost-bc [de0f01] — the name other sessions use to message it (it is not listed below; a message to it would be a message to yourself).

Peer sessions (3):
  drost-fa [f89de1]  ·  interactive  ·  idle  ·  started 35m ago
  drost-e1 [f1abb7]  ·  interactive  ·  busy  ·  started 12m ago
  Factory story-to-review flow [95e2f8]  ·  bg  ·  idle  ·  started 2d ago
";

    #[test]
    fn reads_the_session_listing() {
        let list = SessionList::parse(LISTING);
        assert_eq!(list.own.as_deref(), Some("drost-bc [de0f01]"));
        assert_eq!(list.len(), 3);
        let group = &list.groups[0];
        assert_eq!(group.title, "Peer sessions (3)");
        assert_eq!(group.rows[1].name, "drost-e1");
        assert_eq!(group.rows[1].reference.as_deref(), Some("f1abb7"));
        assert_eq!(
            group.rows[1].details,
            ["interactive", "busy", "started 12m ago"]
        );
        assert!(group.rows[1].is_busy());
        assert_eq!(group.rows[2].name, "Factory story-to-review flow");
        assert!(!group.rows[2].is_busy());
    }

    #[test]
    fn follows_the_first_live_coordination_of_each_turn() {
        let mut follow = Follow::new(2);
        assert!(!follow.observe(2, true), "calls already present");
        assert!(!follow.observe(5, false), "history loaded outside a turn");
        assert!(follow.observe(6, true), "the first live call opens the tab");
        assert!(
            !follow.observe(7, true),
            "later calls leave the user's choice"
        );
        assert!(!follow.observe(7, false), "the turn ends");
        assert!(!follow.observe(7, true), "a new turn without coordination");
        assert!(
            follow.observe(8, true),
            "the next turn's first call opens it again"
        );
    }

    #[test]
    fn unreadable_listings_keep_their_text() {
        let list = SessionList::parse("No other sessions are running.");
        assert_eq!(list.len(), 0);
        assert_eq!(list.own, None);
        assert_eq!(list.raw, "No other sessions are running.");
    }

    #[test]
    fn coordination_tools_become_events_and_others_do_not() {
        let spawn = event(
            "Agent",
            "call-1",
            &json!({"description": "Map tool-call code", "subagent_type": "Explore", "prompt": "…", "run_in_background": true}),
            Outcome::Pending,
            None,
        )
        .unwrap();
        assert_eq!(
            spawn.kind,
            EventKind::Spawn {
                description: Some("Map tool-call code".into()),
                agent_type: Some("Explore".into()),
                background: true,
            }
        );
        let message = event(
            "SendMessage",
            "call-2",
            &json!({"to": "drost-e1", "message": "Tests pass here.\nDetails follow.", "summary": "report"}),
            Outcome::Done,
            Some("\nMessage delivered to drost-e1.\n"),
        )
        .unwrap();
        assert_eq!(
            message.kind,
            EventKind::Message {
                to: "drost-e1".into(),
                text: "Tests pass here.\nDetails follow.".into(),
                notify_when_idle: false,
            }
        );
        assert_eq!(
            message.result.as_deref(),
            Some("Message delivered to drost-e1.")
        );
        let listing = event(
            "ListAgents",
            "call-3",
            &json!({}),
            Outcome::Done,
            Some(LISTING),
        )
        .unwrap();
        assert_eq!(listing.kind, EventKind::Listing { sessions: Some(3) });
        let failed = event(
            "ListAgents",
            "call-4",
            &json!({}),
            Outcome::Failed,
            Some("denied"),
        )
        .unwrap();
        assert_eq!(failed.kind, EventKind::Listing { sessions: None });
        assert_eq!(
            event("Bash", "call-5", &json!({}), Outcome::Done, None),
            None
        );
    }
}
