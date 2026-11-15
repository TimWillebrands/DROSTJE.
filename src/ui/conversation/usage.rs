use crate::harness::{ContextUsage, LimitWindow, UsageLimit};
use gpui_kit::{
    component::{ActiveTheme, Sizable, h_flex, progress::ProgressCircle, tooltip::Tooltip},
    *,
};
use std::time::{Duration, SystemTime};

const USAGE_WARNING_PERCENT: f32 = 80.;

pub(super) fn usage_meters(
    context: Option<ContextUsage>,
    limits: &[UsageLimit],
    now: SystemTime,
    cx: &App,
) -> Vec<AnyElement> {
    let tone = |percent: f32| {
        if percent >= USAGE_WARNING_PERCENT {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        }
    };
    let mut meters = Vec::new();
    if let Some(usage) = context {
        let percent = context_percent(usage);
        let tooltip = format!(
            "Context: {} of {} tokens",
            compact_tokens(usage.used_tokens),
            compact_tokens(usage.max_tokens)
        );
        meters.push(
            h_flex()
                .id("context-usage")
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(tone(percent))
                .child(
                    ProgressCircle::new("context-usage-ring")
                        .xsmall()
                        .color(tone(percent))
                        .value(percent)
                        .accessibility_label(tooltip.clone()),
                )
                .child(format!("{percent:.0}%"))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    for limit in limits {
        let tooltip = limit_description(limit, now);
        meters.push(
            div()
                .id(window_label(limit.window))
                .flex_shrink_0()
                .text_xs()
                .text_color(tone(limit.percent_used))
                .child(format!(
                    "{} {:.0}%",
                    window_label(limit.window),
                    limit.percent_used
                ))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    meters
}

fn context_percent(usage: ContextUsage) -> f32 {
    (usage.used_tokens as f64 * 100. / usage.max_tokens.max(1) as f64).clamp(0., 100.) as f32
}

fn window_label(window: LimitWindow) -> &'static str {
    match window {
        LimitWindow::FiveHour => "5h",
        LimitWindow::SevenDay => "7d",
    }
}

fn limit_description(limit: &UsageLimit, now: SystemTime) -> String {
    let window = match limit.window {
        LimitWindow::FiveHour => "5-hour limit",
        LimitWindow::SevenDay => "7-day limit",
    };
    let reset = match limit.resets_at.map(|at| at.duration_since(now)) {
        Some(Ok(remaining)) => format!(", resets in {}", compact_duration(remaining)),
        Some(Err(_)) => ", reset due".to_owned(),
        None => String::new(),
    };
    format!("{window}: {:.0}% used{reset}", limit.percent_used)
}

fn compact_tokens(tokens: u64) -> String {
    let (value, unit) = match tokens {
        0..1_000 => return tokens.to_string(),
        1_000..1_000_000 => (tokens as f64 / 1_000., "k"),
        _ => (tokens as f64 / 1_000_000., "M"),
    };
    let text = if value < 10. {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
}

fn compact_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextUsage, Duration, LimitWindow, SystemTime, UsageLimit, compact_duration,
        compact_tokens, context_percent, limit_description,
    };

    #[test]
    fn usage_figures_read_compactly() {
        assert_eq!(compact_tokens(950), "950");
        assert_eq!(compact_tokens(1_000), "1k");
        assert_eq!(compact_tokens(8_450), "8.4k");
        assert_eq!(compact_tokens(84_000), "84k");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_duration(Duration::from_secs(59)), "0m");
        assert_eq!(
            compact_duration(Duration::from_secs(2 * 3600 + 14 * 60)),
            "2h 14m"
        );
        assert_eq!(
            compact_duration(Duration::from_secs(3 * 86_400 + 4 * 3600)),
            "3d 4h"
        );
        let context = |used_tokens| ContextUsage {
            used_tokens,
            max_tokens: 200_000,
        };
        assert_eq!(context_percent(context(84_000)), 42.);
        assert_eq!(context_percent(context(250_000)), 100.);

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let limit = |resets_at| UsageLimit {
            window: LimitWindow::FiveHour,
            percent_used: 23.4,
            resets_at,
        };
        assert_eq!(
            limit_description(&limit(Some(now + Duration::from_secs(90 * 60))), now),
            "5-hour limit: 23% used, resets in 1h 30m"
        );
        assert_eq!(
            limit_description(&limit(Some(now - Duration::from_secs(1))), now),
            "5-hour limit: 23% used, reset due"
        );
        assert_eq!(
            limit_description(&limit(None), now),
            "5-hour limit: 23% used"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn usage_meters_render_only_reported_figures(cx: &mut gpui_kit::TestAppContext) {
        use super::usage_meters;
        cx.update(gpui_kit::init);
        let now = SystemTime::UNIX_EPOCH;
        let limits = [
            UsageLimit {
                window: LimitWindow::FiveHour,
                percent_used: 23.,
                resets_at: None,
            },
            UsageLimit {
                window: LimitWindow::SevenDay,
                percent_used: 91.,
                resets_at: None,
            },
        ];
        cx.update(|cx| {
            assert!(usage_meters(None, &[], now, cx).is_empty());
            assert_eq!(usage_meters(None, &limits, now, cx).len(), 2);
            let context = ContextUsage {
                used_tokens: 84_000,
                max_tokens: 200_000,
            };
            assert_eq!(usage_meters(Some(context), &limits, now, cx).len(), 3);
        });
    }
}
