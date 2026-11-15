use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    time::{Duration, Instant},
};

use servo::JSValue;
use url::Url;
use uuid::Uuid;

use super::Page;

pub(super) const CLEAR_STORAGE_PATH: &str = "/.well-known/drostje/clear-storage";
pub(super) const CLEAR_STORAGE_DOCUMENT: &[u8] = b"<!doctype html><title></title>";
const CLEAR_DEADLINE: Duration = Duration::from_secs(5);
const CLEAR_SCRIPT: &str = r#"(() => {
  const key = '__drostjeClearIndexedDb';
  if (globalThis[key] === undefined) {
    globalThis[key] = 'pending';
    indexedDB.databases()
      .then(databases => Promise.all(databases.map(({ name }) => new Promise((resolve, reject) => {
        const request = indexedDB.deleteDatabase(name);
        request.onsuccess = () => resolve();
        request.onerror = () => reject(request.error);
      }))))
      .then(() => { globalThis[key] = 'cleared'; }, error => { globalThis[key] = 'failed: ' + error; });
  }
  return globalThis[key];
})()"#;

pub(super) struct ClosingPage {
    page: Page,
    origins: VecDeque<Url>,
    step: Step,
    deadline: Instant,
}

enum Step {
    Next,
    Loading {
        origin: Url,
        target: Url,
    },
    Clearing {
        origin: Url,
        poll: Rc<RefCell<Poll>>,
    },
}

#[derive(Default)]
struct Poll {
    in_flight: bool,
    status: Option<String>,
}

impl ClosingPage {
    pub(super) fn new(page: Page) -> Option<Self> {
        let origins: VecDeque<Url> = page.delegate.loaded_origins.take().into();
        if origins.is_empty() {
            return None;
        }
        page.delegate.closing.set(true);
        page.view.hide();
        Some(Self {
            page,
            origins,
            step: Step::Next,
            deadline: Instant::now() + CLEAR_DEADLINE,
        })
    }

    pub(super) fn advance(&mut self, live: &HashMap<Uuid, Page>) -> bool {
        if Instant::now() >= self.deadline {
            if let Some(origin) = self.current_origin() {
                eprintln!(
                    "IndexedDB for {origin} was not cleared before the close deadline; it is removed when the browser engine stops"
                );
            }
            return false;
        }
        loop {
            match &self.step {
                Step::Next => {
                    let Some(origin) = self.origins.pop_front() else {
                        return false;
                    };
                    if live
                        .values()
                        .any(|page| page.delegate.has_loaded_origin(&origin))
                    {
                        continue;
                    }
                    let mut target = origin.clone();
                    target.set_path(CLEAR_STORAGE_PATH);
                    self.page.view.load(target.clone());
                    self.step = Step::Loading { origin, target };
                    return true;
                }
                Step::Loading { origin, target } => {
                    let (loaded, failed) = {
                        let state = self.page.delegate.state.borrow();
                        (
                            !state.loading && state.url == target.as_str(),
                            state.error.is_some(),
                        )
                    };
                    if failed {
                        eprintln!("Could not open {target} to clear IndexedDB for {origin}");
                        self.step = Step::Next;
                        continue;
                    }
                    if !loaded {
                        return true;
                    }
                    self.step = Step::Clearing {
                        origin: origin.clone(),
                        poll: Rc::default(),
                    };
                }
                Step::Clearing { origin, poll } => {
                    let status = {
                        let mut state = poll.borrow_mut();
                        if state.in_flight {
                            return true;
                        }
                        state.status.take()
                    };
                    match status.as_deref() {
                        None | Some("pending") => {
                            poll.borrow_mut().in_flight = true;
                            let poll = poll.clone();
                            self.page
                                .view
                                .evaluate_javascript(CLEAR_SCRIPT, move |result| {
                                    let mut poll = poll.borrow_mut();
                                    poll.in_flight = false;
                                    poll.status = Some(match result {
                                        Ok(JSValue::String(status)) => status,
                                        Ok(other) => format!("failed: unexpected {other:?}"),
                                        Err(error) => format!("failed: {error:?}"),
                                    });
                                });
                            return true;
                        }
                        Some("cleared") => {}
                        Some(failure) => {
                            eprintln!("Could not clear IndexedDB for {origin}: {failure}");
                        }
                    }
                    self.step = Step::Next;
                }
            }
        }
    }

    fn current_origin(&self) -> Option<&Url> {
        match &self.step {
            Step::Next => None,
            Step::Loading { origin, .. } | Step::Clearing { origin, .. } => Some(origin),
        }
    }
}

pub(super) fn storage_origin(url: &Url) -> Option<Url> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let origin = url.origin();
    if !origin.is_tuple() {
        return None;
    }
    Url::parse(&format!("{}/", origin.ascii_serialization())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_origins_keep_scheme_host_and_port_only() {
        let origin = |url: &str| storage_origin(&Url::parse(url).unwrap()).map(String::from);
        assert_eq!(
            origin("http://localhost:3000/room/x?as=Tim#y").as_deref(),
            Some("http://localhost:3000/")
        );
        assert_eq!(
            origin("https://dypgangers.eu:443/a").as_deref(),
            Some("https://dypgangers.eu/")
        );
        assert_eq!(origin("file:///tmp/artifact.html"), None);
        assert_eq!(origin("about:blank"), None);
        assert_eq!(origin("data:text/html,hi"), None);
    }
}
