//! Opt-in native component QA with isolated state; never submits provider turns.
use crate::{
    app::{AppState, Bootstrap},
    model::{DraftKey, ThreadId, ThreadKind},
    platform,
    store::Store,
    ui::{
        commands,
        workspace::projects::Row,
        workspace::{Region, Workspace},
    },
};
use gpui_kit::{
    App, AppContext, ClipboardItem, Context, Entity, Focusable, Image, ImageFormat, Modifiers,
    TestAppContext, VisualTestContext, Window, WindowHandle,
    component::{Root, Theme, ThemeMode, WindowExt},
    px, size,
    test::TestWindowExt,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use uuid::Uuid;

mod fixture;
use fixture::*;

mod changes;
mod composer;
mod conversation;
mod editor;
mod factory;
mod focus;
mod network;
mod persistence;
mod review;
mod settings;
mod spaces;
mod terminal;
mod terminal_sidebar;
mod theme;
mod threads;
mod workspace;
