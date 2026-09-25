//! Native frame scheduling acceptance. Uses the real application and renderer;
//! TestAppContext's automatic drawing cannot satisfy these assertions.
use anyhow::{Result, anyhow, ensure};
use gpui::{
    App, Application, AsyncApp, Bounds, Context, FramePerformanceSnapshot, Timer, Window,
    WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, rgb, size,
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Observations {
    rendered_revision: Cell<u32>,
    animation_frames: Cell<u32>,
    callbacks: Cell<u32>,
}

struct Fixture {
    revision: u32,
    animation_remaining: u32,
    observed: Rc<Observations>,
}

impl Render for Fixture {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.observed.rendered_revision.set(self.revision);
        if self.animation_remaining > 0 {
            self.animation_remaining -= 1;
            self.observed
                .animation_frames
                .set(self.observed.animation_frames.get() + 1);
            window.request_animation_frame();
        }
        div().size_full().bg(rgb(0x202020 + self.revision))
    }
}

#[cfg(target_os = "macos")]
fn inject_key(window: WindowHandle<Fixture>, cx: &mut AsyncApp) -> Result<()> {
    use cocoa::{
        appkit::{NSEvent, NSEventModifierFlags, NSEventType},
        base::{id, nil},
        foundation::{NSInteger, NSPoint, NSString},
    };
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let view = window
        .update(cx, |_, window, _| {
            match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
                Ok(RawWindowHandle::AppKit(handle)) => Some(handle.ns_view.as_ptr() as id),
                _ => None,
            }
        })
        .map_err(|_| anyhow!("input window unavailable"))?
        .ok_or_else(|| anyhow!("native input view unavailable"))?;
    // Deliver only to this fixture's native view after releasing the App borrow.
    // Posting to the system event queue could send a key to an unrelated app.
    unsafe {
        let native_window: id = msg_send![view, window];
        let number: NSInteger = msg_send![native_window, windowNumber];
        let characters = NSString::alloc(nil).init_str("a");
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode_(
            nil, NSEventType::NSKeyDown, NSPoint::new(0., 0.),
            NSEventModifierFlags::empty(), 0., number, nil, characters, characters, false, 0,
        );
        let _: () = msg_send![view, keyDown: event];
        let _: () = msg_send![characters, release];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn inject_key(_: WindowHandle<Fixture>, _: &mut AsyncApp) -> Result<()> {
    anyhow::bail!("native frame-demand fixture requires macOS")
}

async fn settle() {
    Timer::after(Duration::from_millis(1500)).await;
}

async fn assert_idle(stage: &str) -> Result<()> {
    let before = FramePerformanceSnapshot::capture();
    Timer::after(Duration::from_millis(250)).await;
    let after = FramePerformanceSnapshot::capture();
    if after.logical_frames != before.logical_frames
        || after.native_vsync_callbacks != before.native_vsync_callbacks
        || after.scene_presents != before.scene_presents
    {
        eprintln!("native_frame_demand_idle stage={stage} before={before:?} after={after:?}");
    }

    ensure!(
        after.logical_frames == before.logical_frames,
        "idle window kept requesting logical frames"
    );
    ensure!(
        after.native_vsync_callbacks == before.native_vsync_callbacks,
        "idle display source kept ticking"
    );
    ensure!(
        after.scene_presents == before.scene_presents,
        "idle window kept presenting"
    );
    Ok(())
}

async fn run(
    window: WindowHandle<Fixture>,
    observed: Rc<Observations>,
    cx: &mut AsyncApp,
) -> Result<()> {
    settle().await;
    assert_idle("launch").await?;

    window
        .update(cx, |view, _, cx| {
            view.revision = 1;
            cx.notify();
        })
        .map_err(|_| anyhow!("notification window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        observed.rendered_revision.get() == 1,
        "entity notification did not wake rendering"
    );
    assert_idle("notification").await?;

    let callback_observed = observed.clone();
    window
        .update(cx, |_, window, _| {
            window.on_next_frame(move |window, _| {
                callback_observed
                    .callbacks
                    .set(callback_observed.callbacks.get() + 1);
                window.on_next_frame(move |_, _| {
                    callback_observed
                        .callbacks
                        .set(callback_observed.callbacks.get() + 1);
                });
            });
        })
        .map_err(|_| anyhow!("callback window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        observed.callbacks.get() == 2,
        "standalone or chained frame callback was stranded"
    );
    assert_idle("callbacks").await?;

    window
        .update(cx, |view, _, cx| {
            view.animation_remaining = 3;
            cx.notify();
        })
        .map_err(|_| anyhow!("animation window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        observed.animation_frames.get() == 3,
        "animation did not complete"
    );
    assert_idle("animation").await?;

    let before = FramePerformanceSnapshot::capture();
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| window.draw(cx).clear())
        .map_err(|_| anyhow!("direct draw window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        FramePerformanceSnapshot::capture().scene_presents > before.scene_presents,
        "direct draw was not presented"
    );
    assert_idle("direct_draw").await?;

    let before = FramePerformanceSnapshot::capture();
    inject_key(window, cx)?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        FramePerformanceSnapshot::capture().scene_presents > before.scene_presents,
        "input did not restart presentation grace"
    );
    let during_grace = FramePerformanceSnapshot::capture();
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        FramePerformanceSnapshot::capture().scene_presents > during_grace.scene_presents,
        "input presentation grace ended prematurely"
    );
    settle().await;
    assert_idle("input_grace").await?;

    cx.update(|cx| cx.hide())
        .map_err(|_| anyhow!("application unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    let before = FramePerformanceSnapshot::capture();
    let callback_observed = observed.clone();
    window
        .update(cx, |view, window, cx| {
            view.revision = 2;
            cx.notify();
            view.revision = 3;
            cx.notify();
            window.on_next_frame(move |_, _| {
                callback_observed
                    .callbacks
                    .set(callback_observed.callbacks.get() + 1)
            });
        })
        .map_err(|_| anyhow!("hidden window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        observed.callbacks.get() == 2,
        "hidden frame callback ran before restoration"
    );
    ensure!(
        FramePerformanceSnapshot::capture().scene_draws == before.scene_draws,
        "hidden notification rendered before restoration"
    );
    cx.update(|cx| cx.activate(true))
        .map_err(|_| anyhow!("application unavailable"))?;
    window
        .update(cx, |_, window, _| window.activate_window())
        .map_err(|_| anyhow!("restoration window unavailable"))?;
    Timer::after(Duration::from_millis(500)).await;
    ensure!(
        observed.rendered_revision.get() == 3 && observed.callbacks.get() == 3,
        "restoration lost scene or callback demand"
    );
    settle().await;
    assert_idle("restoration").await?;
    let other_bounds = window
        .update(cx, |_, window, _| {
            let bounds = window.bounds();
            Bounds::new(
                gpui::point(
                    bounds.origin.x + bounds.size.width + px(20.),
                    bounds.origin.y,
                ),
                size(px(160.), px(160.)),
            )
        })
        .map_err(|_| anyhow!("window bounds unavailable"))?;
    let other = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(other_bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Fixture {
                        revision: 0,
                        animation_remaining: 0,
                        observed: Rc::new(Observations::default()),
                    })
                },
            )
        })
        .map_err(|_| anyhow!("application unavailable"))?
        .map_err(|_| anyhow!("second window creation failed"))?;
    other
        .update(cx, |_, window, _| window.activate_window())
        .map_err(|_| anyhow!("second window unavailable"))?;
    settle().await;
    ensure!(
        !window
            .update(cx, |_, window, _| window.is_window_active())
            .map_err(|_| anyhow!("first window unavailable"))?,
        "first window did not become unfocused"
    );
    assert_idle("two_windows").await?;
    window
        .update(cx, |view, _, cx| {
            view.revision = 4;
            cx.notify();
        })
        .map_err(|_| anyhow!("unfocused window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        observed.rendered_revision.get() == 4,
        "unfocused visible notification was stranded"
    );
    assert_idle("unfocused_notification").await?;
    other
        .update(cx, |_, window, _| window.remove_window())
        .map_err(|_| anyhow!("second window unavailable"))?;
    settle().await;
    assert_idle("second_window_closed").await?;

    // Close during a real frame callback, after its native driver takes the
    // callback. The driver must not restore it or use the destroyed renderer.
    window
        .update(cx, |_, window, _| {
            window.on_next_frame(|window, _| window.remove_window())
        })
        .map_err(|_| anyhow!("closing window unavailable"))?;
    Timer::after(Duration::from_millis(250)).await;
    ensure!(
        window.update(cx, |_, _, _| ()).is_err(),
        "frame callback failed to close its window"
    );
    let closed = FramePerformanceSnapshot::capture();
    ensure!(
        closed.window_sources_created == closed.window_sources_released,
        "closed windows retained frame sources"
    );
    assert_idle("all_windows_closed").await?;
    println!(
        "native_frame_demand idle=pass notify=pass callbacks=pass animation=pass direct_draw=pass input_grace=pass hidden_restore=pass unfocused_visible=pass close_in_callback=pass status=pass"
    );
    Ok(())
}

fn keep_source_awake(window: &Window, enabled: Rc<Cell<bool>>) {
    window.on_next_frame(move |window, _| {
        if enabled.get() {
            keep_source_awake(window, enabled);
        }
    });
}

async fn check_source_state(continuous: bool) -> Result<()> {
    let before = FramePerformanceSnapshot::capture();
    Timer::after(Duration::from_millis(100)).await;
    let after = FramePerformanceSnapshot::capture();
    ensure!(
        (after.native_vsync_callbacks > before.native_vsync_callbacks) == continuous,
        "latency fixture source state did not settle"
    );
    Ok(())
}

async fn await_presentation(
    started: Instant,
    previous_presents: u64,
    revision: Option<(&Observations, u32)>,
) -> Result<u128> {
    loop {
        if FramePerformanceSnapshot::capture().scene_presents > previous_presents
            && revision
                .is_none_or(|(observed, revision)| observed.rendered_revision.get() == revision)
        {
            return Ok(started.elapsed().as_micros());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(2),
            "latency fixture presentation deadline exceeded"
        );
        Timer::after(Duration::from_millis(1)).await;
    }
}

async fn run_latency(
    window: WindowHandle<Fixture>,
    observed: Rc<Observations>,
    cx: &mut AsyncApp,
) -> Result<()> {
    const SAMPLES: u32 = 16;
    let mut samples = Vec::with_capacity(SAMPLES as usize);
    for sample in 0..SAMPLES {
        let continuous = sample % 2 == 0;
        let enabled = Rc::new(Cell::new(continuous));
        if continuous {
            let enabled = enabled.clone();
            window
                .update(cx, |_, window, _| keep_source_awake(window, enabled))
                .map_err(|_| anyhow!("continuous control window unavailable"))?;
        }
        // Deterministic phase variation avoids sampling only one refresh phase.
        let settle_duration = Duration::from_millis(1500 + u64::from((sample * 13) % 31));
        Timer::after(settle_duration).await;
        check_source_state(continuous).await?;
        let revision = 100 + sample;
        let before = FramePerformanceSnapshot::capture();
        let started = Instant::now();
        window
            .update(cx, |view, _, cx| {
                view.revision = revision;
                cx.notify();
            })
            .map_err(|_| anyhow!("latency notification window unavailable"))?;
        let notify_us =
            await_presentation(started, before.scene_presents, Some((&observed, revision))).await?;

        Timer::after(settle_duration).await;
        check_source_state(continuous).await?;
        let before = FramePerformanceSnapshot::capture();
        let started = Instant::now();
        inject_key(window, cx)?;
        let input_us = await_presentation(started, before.scene_presents, None).await?;
        enabled.set(false);
        let source = if continuous { "continuous" } else { "idle" };
        println!(
            "native_frame_latency sample={sample} source={source} notify_to_present_us={notify_us} input_to_present_us={input_us}"
        );
        samples.push((continuous, notify_us, input_us));
    }
    for continuous in [true, false] {
        let mut notify = samples
            .iter()
            .filter(|sample| sample.0 == continuous)
            .map(|sample| sample.1)
            .collect::<Vec<_>>();
        let mut input = samples
            .iter()
            .filter(|sample| sample.0 == continuous)
            .map(|sample| sample.2)
            .collect::<Vec<_>>();
        notify.sort_unstable();
        input.sort_unstable();
        let source = if continuous { "continuous" } else { "idle" };
        let count = notify.len();
        let notify_median = (notify[count / 2 - 1] + notify[count / 2]) / 2;
        let input_median = (input[count / 2 - 1] + input[count / 2]) / 2;
        println!(
            "native_frame_latency_summary source={source} samples={count} notify_median_us={notify_median} notify_min_us={} notify_max_us={} input_median_us={input_median} input_min_us={} input_max_us={}",
            notify[0],
            notify[count - 1],
            input[0],
            input[count - 1]
        );
    }
    settle().await;
    assert_idle("latency_complete").await?;
    println!("native_frame_latency status=pass");
    Ok(())
}

fn main() {
    let latency_mode =
        std::env::var_os("GPUI_FRAME_DEMAND_LATENCY").is_some_and(|value| value == "1");
    let success = Rc::new(Cell::new(false));
    let result = success.clone();
    Application::new().run(move |cx: &mut App| {
        let observed = Rc::new(Observations::default());
        let bounds = Bounds::centered(None, size(px(480.), px(320.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Fixture {
                        revision: 0,
                        animation_remaining: 0,
                        observed: observed.clone(),
                    })
                },
            )
            .expect("native fixture window creation failed");
        cx.activate(true);
        cx.spawn(async move |cx| {
            let check = if latency_mode {
                run_latency(window, observed, cx).await
            } else {
                run(window, observed, cx).await
            };
            match check {
                Ok(()) => result.set(true),
                Err(error) => {
                    eprintln!("native_frame_demand status=fail reason={error}");
                    // AppKit's terminate: exits with status 0 instead of returning
                    // from Application::run, so fail before asking AppKit to quit.
                    std::process::exit(1);
                }
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    if !success.get() {
        std::process::exit(1);
    }
}
