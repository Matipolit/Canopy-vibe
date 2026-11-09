# Bug reports

## BUG-003: Multiple Duckers share the Smoothing disclosure's UI ID

Reported: 9 November 2026. Status: Fixed and installed on 9 November 2026.

**Steps to reproduce:** Put two Duckers in one effects rack and click either
Smoothing disclosure.

**Actual behavior:** The disclosures share a persistent egui ID, interfere with
each other's state, and appear unresponsive. The original single-Ducker UI test
does not catch this; a two-Ducker test reproduces the collision before the fix.

**Expected behavior:** Each Ducker independently opens and closes its smoothing
controls, and edits affect only that device.

**Fix:** Give the disclosures device-specific IDs through `Panel::id`, including
Opto smoothing and sampler Click protection. Verified with physical pointer
clicks on two Duckers, independent expansion/collapse, editing the second
device's detector release, and visual inspection of the rendered panels.

## BUG-001: Track context menu is intercepted by a clip behind the track list

Reported: 8 November 2026. Status: Fixed locally on 8 November 2026.

**Steps to reproduce**

1. Place a MIDI or audio clip on the playlist.
2. Scroll the playlist horizontally until the clip lies behind the left-side track list.
3. Right-click the corresponding track in the left-side track list, over the area with the hidden clip underneath.

**Actual behavior:** The MIDI or audio clip's context menu opens instead of the track's context menu.

**Expected behavior:** Right-clicking the track list opens the track's context menu. Clips hidden behind the track list must not receive that click.

**Acceptance criteria:** Verify both MIDI and audio clips. Right-clicking a track header opens the correct track menu regardless of clips underneath it or horizontal scroll position; right-clicking a visible clip in the playlist still opens its clip menu.

**Fix:** Clip lane interaction regions to the lane boundary, including automation lanes, so hidden widgets cannot intercept track-header clicks. Regression tests cover track menus over hidden MIDI and audio clips and menus on visible clips.

## BUG-002: Opening a MIDI clip does not immediately focus the piano roll

Reported: 8 November 2026. Status: Fixed locally on 8 November 2026.

**Steps to reproduce**

1. Focus the playlist with a MIDI clip available to edit.
2. Open the MIDI clip for editing in the piano roll.
3. Immediately press Ctrl+A without first clicking inside the piano roll.

**Actual behavior:** Keyboard focus remains on the playlist. Ctrl+A selects all tracks in the playlist, and other shortcuts continue to target the playlist.

**Expected behavior:** Opening a MIDI clip for editing immediately transfers keyboard focus to the piano roll. Ctrl+A selects notes in the opened clip, and other editor shortcuts act on the piano roll.

**Acceptance criteria:** Immediately after opening a MIDI clip, Ctrl+A selects its notes without changing playlist selection. Other shortcuts shared by the playlist and piano roll target the piano roll until focus is explicitly moved elsewhere.

**Fix:** Opening a MIDI clip transfers shortcut focus to the piano roll after pointer focus is processed. A regression test opens a clip, selects and deletes its notes using the keyboard, then verifies that clicking the playlist restores its shortcuts.
