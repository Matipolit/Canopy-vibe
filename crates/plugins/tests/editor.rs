//! Plugin editors in windows of their own, run by the host: the test
//! plugins' X11 editors only work if the host's timers, file-descriptor
//! watching (CLAP timer-support and posix-fd-support, VST3 `IRunLoop`),
//! resize requests and window handling do.
//!
//! These open windows, so they're ignored by default; run them on a
//! desktop (XWayland is fine):
//! `cargo test -p plugins --test editor -- --ignored`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::*;
use plugins::{EditorOptions, Host, PluginEvent};

fn has_display() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_some()
}

#[test]
#[ignore = "opens windows: needs an X display"]
fn editors_run_from_the_hosts_timers_and_descriptors() {
    if !has_display() {
        eprintln!("no X display; skipped");
        return;
    }
    for format in FORMATS {
        let host = Host::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        host.set_waker(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let mut gain = format.load(&host, format.gain);
        assert!(gain.has_editor(), "{format:?}");
        let options = EditorOptions {
            title: format!("canopy test editor ({:?})", format.format),
            keep_on_top: true,
        };
        gain.open_editor(&options).unwrap();
        assert!(gain.editor_is_open());
        gain.set_editor_keep_on_top(false);
        let until = Instant::now() + Duration::from_millis(800);
        while Instant::now() < until {
            host.idle();
            std::thread::sleep(Duration::from_millis(5));
        }
        let ticks = gain.param_value(GUI_TICKS).unwrap();
        assert!(
            ticks >= 10.0,
            "{format:?}: the host ran the editor's timer ({ticks} ticks)"
        );
        assert!(
            gain.param_value(GUI_FD_EVENTS).unwrap() >= 1.0,
            "{format:?}: and its descriptor"
        );
        assert_eq!(
            gain.param_value(GUI_PARENT_WIDTH),
            Some(400.0),
            "{format:?}: the host resized its window as the editor asked"
        );
        assert_eq!(gain.param_value(GUI_OPEN), Some(1.0));
        assert!(
            wakes.load(Ordering::SeqCst) > 0,
            "{format:?}: the waker fired"
        );

        gain.close_editor();
        assert!(!gain.editor_is_open());
        assert_eq!(
            gain.param_value(GUI_OPEN),
            Some(0.0),
            "{format:?}: the editor was torn down"
        );
        host.idle();
        assert!(!gain.take_events().contains(&PluginEvent::EditorClosed));

        // Again, and closed by dropping the instance.
        gain.open_editor(&EditorOptions::titled("again")).unwrap();
        host.idle();
        drop(gain);
        host.idle();
    }
}
