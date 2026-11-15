//! Review sidebar: tracker failures and review threads.
use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; Review sidebar states, no tracker call succeeds"]
async fn review_sidebar_shows_tracker_failures_and_review_threads(cx: &mut TestAppContext) {
    use crate::model::{PullRequestLink, PullRequestState, Worktree};
    use crate::tracker::{ReviewComment, ReviewStatus, ReviewThread, ReviewThreads};
    let fixture = open_fixture(cx, None).await;
    let conversation = cx.update(|cx| {
        fixture.state.update(cx, |state, cx| {
            let project = state.project().unwrap().clone();
            let id = Uuid::new_v4();
            state.save_worktree(Worktree {
                id,
                project_id: project.id,
                // The project checkout stands in for the worktree directory: its `origin` is
                // not on Azure DevOps, so a real read fails without contacting a tracker.
                path: project.root.clone(),
                codename: "quiet-forest".into(),
                branch: "drostje/review".into(),
                base: "main".into(),
                named: true,
                created_by: Uuid::new_v4(),
                created_at: platform::now(),
                removed_at: None,
            });
            state.new_conversation_in_workspace(Some(id)).unwrap();
            let mut thread = state.thread().unwrap().clone();
            thread.pull_request = Some(PullRequestLink {
                id: 3170,
                url: "https://dev.azure.com/o/p/_git/r/pullrequest/3170".into(),
                title: "Fix upload".into(),
                state: PullRequestState::Active,
                work_items: vec![2596],
                archived_on_close: false,
            });
            let id = thread.id;
            state.save_thread(thread);
            cx.notify();
            id
        })
    });
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        }
    };

    cx.dispatch_action(fixture.window.into(), commands::ToggleReview);
    until(cx, "failed review read", Duration::from_secs(10), |cx| {
        fixture
            .state
            .read(cx)
            .review_state(conversation)
            .is_some_and(|review| !review.loading && matches!(review.result, Some(Err(_))))
    })
    .await;
    draw(cx);
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    assert!(visual.debug_bounds("review-sidebar").is_some());
    assert!(visual.debug_bounds("review-error").is_some());
    assert!(visual.debug_bounds("open-pull-request").is_some());

    cx.update(|cx| {
        fixture.state.update(cx, |state, cx| {
            let review = state.pull_requests.reviews.get_mut(&conversation).unwrap();
            let comment = |body: &str| ReviewComment {
                id: 1,
                author: "Reviewer".into(),
                body: body.into(),
                truncated: false,
                published: "2026-10-20T10:00:00Z".into(),
            };
            review.result = Some(Ok(ReviewThreads {
                threads: vec![
                    ReviewThread {
                        id: 11,
                        status: ReviewStatus::Active,
                        file: Some("/src/upload.rs".into()),
                        line: Some(42),
                        outdated: false,
                        comments: vec![comment("Handle the empty upload")],
                        total_comments: 3,
                    },
                    ReviewThread {
                        id: 12,
                        status: ReviewStatus::Fixed,
                        file: None,
                        line: None,
                        outdated: false,
                        comments: vec![comment("Looks good")],
                        total_comments: 1,
                    },
                ],
                truncated: false,
            }));
            cx.emit(crate::app::PresentationChange::Review(conversation));
            cx.notify();
        })
    });
    draw(cx);
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    assert!(visual.debug_bounds("review-error").is_none());
    let open = visual.debug_bounds("review-thread-11").unwrap();
    let resolved = visual.debug_bounds("review-thread-12").unwrap();
    assert!(open.origin.y < resolved.origin.y, "open threads come first");

    cx.dispatch_action(fixture.window.into(), commands::ToggleReview);
    until(cx, "hidden review sidebar", Duration::from_secs(5), |cx| {
        !fixture.state.read(cx).thread_sidebar_open(conversation)
    })
    .await;
    draw(cx);
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    assert!(visual.debug_bounds("review-sidebar").is_none());
}
