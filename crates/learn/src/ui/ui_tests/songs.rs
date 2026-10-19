//! Songs from MIDI files, driven by a MIDI keyboard.

use eframe::egui::{Event, Key, Modifiers, PointerButton, pos2};

use super::{Rig, peak, temp_dir};
use crate::smf;
use crate::song::{Hands, LEAD_IN, Mode};
use crate::theory::Hand;
use crate::ui::app::View;

/// At 120 BPM: the right hand plays E D C D on the first four beats, the
/// left hand holds a C and then a G.
fn write_song(name: &str) -> std::path::PathBuf {
    let mut w = smf::Writer::new(480);
    w.track("").tempo(0, 120.0).time_signature(0, 4, 4);
    w.track("Melody")
        .note(0, 0, 400, 64, 90)
        .note(0, 480, 400, 62, 90)
        .note(0, 960, 400, 60, 90)
        .note(0, 1440, 400, 62, 90);
    w.track("Bass")
        .note(1, 0, 900, 48, 70)
        .note(1, 960, 900, 43, 70);
    let path = temp_dir(name).join("hands.mid");
    std::fs::write(&path, w.bytes()).unwrap();
    path
}

fn open(rig: &mut Rig, path: &std::path::Path) {
    let app = rig.app_mut();
    app.songs.open_file(path, &mut app.shared);
    rig.frames(2);
}

fn position(rig: &Rig) -> f64 {
    rig.app().songs.player.as_ref().unwrap().position()
}

#[test]
fn wait_mode_waits_for_the_right_notes() {
    let mut rig = Rig::new("song_wait");
    rig.view(View::Songs);
    let path = write_song("wait");
    open(&mut rig, &path);
    {
        let player = rig.app().songs.player.as_ref().unwrap();
        assert_eq!(player.song().parts.len(), 2);
        assert_eq!(player.mode, Mode::Wait);
        assert_eq!(player.hands, Hands::Right);
        assert_eq!(
            player.hand(0),
            Hand::Left,
            "the bass track is the left hand"
        );
    }
    rig.screenshot("loaded");

    rig.key(Modifiers::NONE, Key::Space);
    assert!(rig.app().songs.is_playing());
    rig.wait(1.0);
    rig.screenshot("count_in");
    rig.wait(2.0);
    // Held at the first right-hand note, the left hand waiting with it.
    assert_eq!(position(&rig), 0.0);
    {
        let player = rig.app().songs.player.as_ref().unwrap();
        assert!(player.is_waiting());
        assert_eq!(player.waiting_for(), [64]);
        assert_eq!(player.sounding().count(), 0);
    }
    assert_eq!(peak(&rig.take_audio()), 0.0, "nothing plays yet");
    rig.screenshot("waiting");

    // A wrong note: still waiting.
    rig.tap(65);
    rig.wait(0.5);
    assert_eq!(position(&rig), 0.0);
    assert_eq!(rig.app().songs.player.as_ref().unwrap().stats.wrong, 1);

    // The right one: on it goes, and the app plays the left hand.
    rig.tap(64);
    rig.wait(0.25);
    assert!(position(&rig) > 0.2);
    assert_eq!(
        rig.app()
            .songs
            .player
            .as_ref()
            .unwrap()
            .sounding()
            .collect::<Vec<_>>(),
        [(48, Hand::Left)]
    );
    rig.screenshot("moving");
    // Next it stops at D.
    rig.wait(0.5);
    assert_eq!(position(&rig), 0.5);
    assert_eq!(rig.app().songs.player.as_ref().unwrap().waiting_for(), [62]);
    for pitch in [62, 60, 62] {
        rig.tap(pitch);
        rig.wait(0.4);
    }
    rig.wait(2.0);
    let player = rig.app().songs.player.as_ref().unwrap();
    assert!(player.is_finished());
    assert_eq!(player.stats.hits, 4);
    rig.screenshot("finished");
    let record = &rig.app().shared.progress.songs["hands"];
    assert!((record.bests["Wait · Right hand"].score - 0.8).abs() < 1e-9);
    assert!(record.seconds > 3.0, "practice time counted");
}

#[test]
fn play_along_scores_timing() {
    let mut rig = Rig::new("song_along");
    rig.view(View::Songs);
    {
        let app = rig.app_mut();
        app.songs.settings.mode = Mode::PlayAlong;
        app.songs.open_builtin("Ode to Joy", &mut app.shared);
    }
    rig.frames(2);
    let notes: Vec<(f64, u8)> = {
        let player = rig.app().songs.player.as_ref().unwrap();
        assert_eq!(player.mode, Mode::PlayAlong);
        player
            .song()
            .notes
            .iter()
            .enumerate()
            .filter(|(i, _)| player.hand(*i) == Hand::Right)
            .map(|(_, n)| (n.start, n.pitch))
            .collect()
    };
    rig.key(Modifiers::NONE, Key::Space);
    let start = rig.app().shared.now() + LEAD_IN;
    // Play the first twelve notes on time, skip one, play two a bit late.
    for (k, (at, pitch)) in notes.iter().take(16).enumerate() {
        if k == 12 {
            continue;
        }
        let lateness = if k >= 14 { 0.09 } else { 0.0 };
        let wait = start + at + lateness - rig.app().shared.now();
        rig.wait(wait.max(0.0));
        rig.note_on(*pitch);
        rig.frames(1);
        rig.note_off(*pitch);
        if k == 8 {
            rig.screenshot("playing");
        }
    }
    rig.wait(0.5);
    let player = rig.app().songs.player.as_ref().unwrap();
    assert_eq!(player.stats.hits, 15);
    assert_eq!(player.stats.misses, 1);
    let mean = player.stats.timing.mean_abs().unwrap();
    assert!(mean < 30.0, "mostly on time: {mean}");
    rig.screenshot("scored");
}

#[test]
fn listen_seek_and_loop() {
    let mut rig = Rig::new("song_listen");
    rig.view(View::Songs);
    {
        let app = rig.app_mut();
        app.songs.settings.mode = Mode::Listen;
        app.songs.open_builtin("Twinkle, Twinkle", &mut app.shared);
    }
    rig.frames(2);
    rig.key(Modifiers::NONE, Key::Space);
    rig.wait(LEAD_IN + 1.0);
    assert!(peak(&rig.take_audio()) > 0.01, "the app plays the song");
    rig.screenshot("listening");

    // Clicking the bar halfway seeks there.
    let bar = rig.app().songs.bar_rect.expect("the progress bar is shown");
    let duration = rig.app().songs.player.as_ref().unwrap().song().duration;
    rig.click(bar.center());
    let at = position(&rig);
    assert!(
        (at - duration / 2.0).abs() < duration * 0.05,
        "{at} of {duration}"
    );

    // Shift+drag over a stretch of it loops that.
    let from = pos2(bar.left() + bar.width() * 0.25, bar.center().y);
    let to = pos2(bar.left() + bar.width() * 0.4, bar.center().y);
    rig.event(Event::ModifiersChanged(Modifiers::SHIFT));
    rig.event(Event::PointerMoved(from));
    rig.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::SHIFT,
    });
    for i in 1..=6 {
        let p = from + (to - from) * (i as f32 / 6.0);
        rig.event(Event::PointerMoved(p));
    }
    rig.event(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::SHIFT,
    });
    rig.event(Event::ModifiersChanged(Modifiers::NONE));
    let (a, b) = rig
        .app()
        .songs
        .player
        .as_ref()
        .unwrap()
        .looped
        .expect("looping");
    assert!((a - duration * 0.25).abs() < duration * 0.03);
    assert!((b - duration * 0.4).abs() < duration * 0.03);
    rig.screenshot("looped");
    // It comes back round.
    rig.wait(LEAD_IN + (b - a) + 0.5);
    let at = position(&rig);
    assert!(at >= a - 0.01 && at < b, "{a} <= {at} < {b}");
    // Ctrl+L turns it off.
    rig.key(Modifiers::COMMAND, Key::L);
    assert!(rig.app().songs.player.as_ref().unwrap().looped.is_none());
    // Leaving the view pauses.
    rig.view(View::Stats);
    assert!(!rig.app().songs.is_playing());
    rig.screenshot("progress");
}
