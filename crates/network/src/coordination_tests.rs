#[test]
fn discovery_uses_self_description_but_cannot_cross_spaces_or_replace_runtime_context() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Reviewer");
        let outsider = peer(Uuid::new_v4(), "Other reviewer");
        fixture
            .join(&[a.clone(), b.clone(), outsider.clone()])
            .await;
        for member in [&b, &outsider] {
            fixture
                .network
                .call(
                    token(member),
                    Action::SetIdentity {
                        activity: Some("Reviewing the network change".into()),
                        tags: Some(vec!["role:reviewer".into(), "issue:123".into()]),
                    },
                )
                .await
                .unwrap();
        }
        for selector in [
            "role=reviewer issue=123",
            "role=rev* ~network",
            "activity=network",
        ] {
            let found = fixture
                .network
                .call(
                    token(&a),
                    Action::Discover {
                        selector: selector.into(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(found["members"].as_array().unwrap().len(), 1);
            assert_eq!(found["members"][0]["thread"], json!(b.thread));
        }
        assert!(
            fixture
                .network
                .call(
                    token(&a),
                    Action::SetIdentity {
                        activity: None,
                        tags: Some(vec!["project:other".into()])
                    }
                )
                .await
                .is_err()
        );
        let sent = fixture
            .network
            .call(
                token(&a),
                Action::Multicast {
                    selector: "role=reviewer".into(),
                    body: "Please inspect".into(),
                    priority: Priority::Fyi,
                },
            )
            .await
            .unwrap();
        assert_eq!(sent["message"]["deliveries"].as_array().unwrap().len(), 1);
        assert_eq!(
            sent["message"]["deliveries"][0]["recipient"],
            json!(b.thread)
        );
        fixture
            .network
            .sync(vec![a.clone(), b.clone(), outsider.clone()])
            .await
            .unwrap();
        assert_eq!(
            fixture
                .saved()
                .await
                .members
                .iter()
                .find(|member| member.thread == b.thread)
                .unwrap()
                .tags,
            ["role:reviewer", "issue:123"]
        );
        fixture.network.set_joined(b.thread, false).await.unwrap();
        assert!(
            fixture
                .network
                .call(
                    token(&b),
                    Action::SetIdentity {
                        activity: Some("Unauthorized".into()),
                        tags: None
                    }
                )
                .await
                .is_err()
        );
    });
}

#[test]
fn priorities_and_inbound_policy_control_push_without_replaying_consumed_mail() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let mut b = peer(a.space, "Recipient");
        b.status = "Working".into();
        fixture.join(&[a.clone(), b.clone()]).await;
        let mut ids = Vec::new();
        for priority in [Priority::Normal, Priority::Fyi, Priority::Urgent] {
            let result = fixture
                .network
                .call(
                    token(&a),
                    Action::Send {
                        recipient: b.thread,
                        body: priority.label().into(),
                        priority,
                    },
                )
                .await
                .unwrap();
            ids.push(serde_json::from_value::<Uuid>(result["message"]["id"].clone()).unwrap());
        }
        let urgent = fixture.network.claim(token(&b)).await.unwrap().unwrap();
        assert_eq!(urgent.messages.len(), 1);
        assert_eq!(urgent.messages[0].id, ids[2]);
        fixture
            .network
            .finish(urgent.attempt, DeliveryOutcome::Delivered)
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Hold,
                },
            )
            .await
            .unwrap();
        b.status = "Idle".into();
        fixture
            .network
            .sync(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Accept,
                },
            )
            .await
            .unwrap();
        let normal = fixture.network.claim(token(&b)).await.unwrap().unwrap();
        assert_eq!(normal.messages.len(), 1);
        assert_eq!(normal.messages[0].id, ids[0]);
        fixture
            .network
            .finish(normal.attempt, DeliveryOutcome::Delivered)
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        let inbox = fixture
            .network
            .call(token(&b), Action::Inbox)
            .await
            .unwrap();
        assert_eq!(inbox["messages"][0]["id"], json!(ids[1]));
        assert_eq!(
            inbox["messages"][0]["deliveries"][0]["state"]["status"],
            "read"
        );
        assert!(
            fixture
                .network
                .call(token(&b), Action::Inbox)
                .await
                .unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Refuse,
                },
            )
            .await
            .unwrap();
        let refused = fixture.send(&a, &b, "Refuse this").await;
        let saved = fixture.saved().await;
        assert!(matches!(
            saved
                .messages
                .iter()
                .find(|message| message.id == refused)
                .unwrap()
                .deliveries[0]
                .state,
            DeliveryState::Refused { .. }
        ));
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Accept,
                },
            )
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        validate_snapshot(&fixture.saved().await).unwrap();
    });
}

#[test]
fn ask_is_correlated_and_returns_a_reply_without_a_second_native_delivery() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Reviewer");
        let c = peer(a.space, "Unrelated");
        fixture.join(&[a.clone(), b.clone(), c.clone()]).await;
        let waiting = fixture
            .network
            .begin_call(
                token(&a),
                Action::Ask {
                    recipient: b.thread,
                    body: "Review?".into(),
                    timeout_ms: 2000,
                },
            )
            .await
            .unwrap();
        fixture.network.flush().await.unwrap();
        let question = fixture.saved().await.messages[0].clone();
        assert!(question.expect_reply);
        assert_eq!(question.priority, Priority::Urgent);
        assert!(
            fixture
                .network
                .call(
                    token(&c),
                    Action::Reply {
                        message: question.id,
                        body: "Spoof".into(),
                        priority: Priority::Normal
                    }
                )
                .await
                .is_err()
        );
        fixture
            .network
            .call(
                token(&b),
                Action::Reply {
                    message: question.id,
                    body: "Reviewed".into(),
                    priority: Priority::Normal,
                },
            )
            .await
            .unwrap();
        let result = waiting.recv().await.unwrap().unwrap();
        assert_eq!(result["event"], "reply");
        assert_eq!(result["question"]["id"], json!(question.id));
        assert_eq!(result["messages"][0]["reply_to"], json!(question.id));
        assert_eq!(result["messages"][0]["exchange"], json!(question.id));
        assert!(fixture.network.claim(token(&a)).await.unwrap().is_none());
        let history = fixture
            .network
            .call(
                token(&a),
                Action::History {
                    message: Some(question.id),
                    query: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(history["messages"].as_array().unwrap().len(), 2);
        let private = fixture
            .network
            .call(
                token(&c),
                Action::History {
                    message: Some(question.id),
                    query: None,
                },
            )
            .await;
        assert!(private.is_err());
        validate_snapshot(&fixture.saved().await).unwrap();
    });
}

#[test]
fn wait_timeout_preserves_the_question_and_late_reply_stays_deliverable() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Reviewer");
        fixture.join(&[a.clone(), b.clone()]).await;
        let result = fixture
            .network
            .call(
                token(&a),
                Action::Ask {
                    recipient: b.thread,
                    body: "Question".into(),
                    timeout_ms: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(result["event"], "timeout");
        let id = serde_json::from_value(result["question"]["id"].clone()).unwrap();
        assert_eq!(fixture.saved().await.messages.len(), 1);
        fixture
            .network
            .call(
                token(&b),
                Action::Reply {
                    message: id,
                    body: "Late reply".into(),
                    priority: Priority::Normal,
                },
            )
            .await
            .unwrap();
        let batch = fixture.network.claim(token(&a)).await.unwrap().unwrap();
        assert_eq!(batch.messages[0].reply_to, Some(id));
        assert_eq!(batch.messages.len(), 1);
        fixture
            .network
            .finish(
                batch.attempt,
                DeliveryOutcome::Unknown("Lost receipt".into()),
            )
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&a)).await.unwrap().is_none());
        assert!(
            fixture
                .network
                .call(token(&a), Action::Inbox)
                .await
                .unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn waits_recheck_runtime_authorization_and_membership_and_do_not_poll() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let mut b = peer(a.space, "Worker");
        b.status = "Working".into();
        fixture.join(&[a.clone(), b.clone()]).await;
        let idle = fixture
            .network
            .begin_call(
                token(&a),
                Action::Wait {
                    event: format!("idle:{}", b.thread),
                    timeout_ms: 2000,
                },
            )
            .await
            .unwrap();
        fixture.network.flush().await.unwrap();
        assert!(idle.try_recv().is_err());
        b.status = "Idle".into();
        fixture
            .network
            .sync(vec![a.clone(), b.clone()])
            .await
            .unwrap();
        assert_eq!(idle.recv().await.unwrap().unwrap()["event"], "idle");
        for revoke in [false, true] {
            let waiting = fixture
                .network
                .begin_call(
                    token(&a),
                    Action::Wait {
                        event: "message".into(),
                        timeout_ms: 2000,
                    },
                )
                .await
                .unwrap();
            fixture.network.flush().await.unwrap();
            if revoke {
                fixture.network.set_joined(a.thread, false).await.unwrap();
            } else {
                let mut revoked = a.clone();
                revoked.token = Some(Uuid::new_v4());
                fixture
                    .network
                    .sync(vec![revoked, b.clone()])
                    .await
                    .unwrap();
            }
            assert!(waiting.recv().await.unwrap().is_err());
            fixture.join(&[a.clone(), b.clone()]).await;
        }
        for event in [
            format!("idle:{}", Uuid::new_v4()),
            format!("idle:{}", a.thread),
            format!("reply:{}", Uuid::new_v4()),
        ] {
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Wait {
                            event,
                            timeout_ms: 20
                        }
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn cancelled_waiters_do_not_consume_mail_and_busy_waiters_do_not_block_members() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Worker");
        fixture.join(&[a.clone(), b.clone()]).await;
        let waiting = fixture
            .network
            .begin_call(
                token(&a),
                Action::Wait {
                    event: "message".into(),
                    timeout_ms: 2000,
                },
            )
            .await
            .unwrap();
        fixture.network.flush().await.unwrap();
        assert_eq!(
            fixture
                .network
                .call(token(&b), Action::Members)
                .await
                .unwrap()["members"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            fixture
                .network
                .call(
                    token(&a),
                    Action::Ask {
                        recipient: b.thread,
                        body: "Must not send".into(),
                        timeout_ms: 10
                    }
                )
                .await
                .is_err()
        );
        assert!(fixture.saved().await.messages.is_empty());
        drop(waiting);
        fixture.send(&b, &a, "Keep queued").await;
        assert!(fixture.network.claim(token(&a)).await.unwrap().is_some());
    });
}

#[test]
fn old_messages_keep_urgent_delivery_and_new_sends_default_to_normal() {
    let action: Action =
        serde_json::from_value(json!({"action":"send","recipient":Uuid::new_v4(),"body":"Hello"}))
            .unwrap();
    assert!(matches!(
        action,
        Action::Send {
            priority: Priority::Normal,
            ..
        }
    ));
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Worker");
        fixture.join(&[a.clone(), b.clone()]).await;
        fixture.send(&a, &b, "Legacy").await;
        let mut saved = serde_json::to_value(fixture.saved().await).unwrap();
        saved["messages"][0]
            .as_object_mut()
            .unwrap()
            .remove("priority");
        let old = read_saved(&saved).unwrap();
        assert_eq!(old.messages[0].priority, Priority::Urgent);
        validate_snapshot(&old).unwrap();
    });
}

#[test]
fn refusing_between_claim_and_dispatch_closes_the_unwritten_attempt() {
    smol::block_on(async {
        let fixture = Fixture::new();
        let a = peer(Uuid::new_v4(), "Author");
        let b = peer(a.space, "Worker");
        fixture.join(&[a.clone(), b.clone()]).await;
        fixture.send(&a, &b, "Claimed but not written").await;
        let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Refuse,
                },
            )
            .await
            .unwrap();
        fixture
            .network
            .finish(batch.attempt, DeliveryOutcome::Deferred)
            .await
            .unwrap();
        fixture
            .network
            .call(
                token(&b),
                Action::SetInbound {
                    policy: InboundPolicy::Accept,
                },
            )
            .await
            .unwrap();
        assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        let saved = fixture.saved().await;
        assert!(matches!(
            saved.messages[0].deliveries[0].state,
            DeliveryState::Refused { .. }
        ));
        assert_eq!(saved.messages[0].deliveries[0].attempt, None);
        validate_snapshot(&saved).unwrap();
    });
}
