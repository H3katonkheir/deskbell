---
name: deskbell
description: Etiquette for using the user's physical mouse and keyboard through the deskbell widget (MCP server "deskbell"). Use BEFORE any action that moves the pointer, clicks or types on the user's real desktop (computer use, pyautogui, AutoHotkey, xdotool, or the request_control/mouse_*/type_text/press_keys tools), since the user may be using the computer at the same time.
when_to_use: The user asks you to click, type into, drag, scroll or operate a GUI app on their machine; you are about to use a computer-use, mouse or keyboard tool; a tool returns "NOT HOLDING", "WAITING" or "DENIED"; or the user mentions the bell. Not needed for headless browsers, VMs or sandboxes with no human at the computer, or for shell commands.
---

# Using the user's mouse and keyboard via deskbell

The user shares their physical mouse and keyboard with you. A small bell on their screen
arbitrates access: you ring it with `request_control`, the user sees your name and task in a
speech bubble and either clicks the bell (allow), right-clicks it (deny), or leaves the computer
alone for a while (auto-allow). Only one agent has control at a time; the rest wait in a
queue. Access is a short **lease** (~60 s) that you renew. **The moment the user touches the
mouse (or, on most setups, the keyboard), you lose control.**

The bell's tools come from the `deskbell` MCP server. If they aren't available, don't operate
the user's mouse or keyboard silently: tell the user the bell isn't connected and ask how to
proceed. If a call says the widget isn't running, ask the user to start `deskbell`.

## Before you ring

1. Do everything that doesn't need input first: read files, take screenshots, work out the
   steps, coordinates and text to type. Screenshots and `get_screen_info` don't need a lease.
   Keep the lease time short.
2. Call `control_status` once. Its last line says which input tools you may use:
   - *"Your own mouse/keyboard tools are allowed…"*: the machine tells physical input apart
     from software input, so your own computer-use tool is fine while you hold the lease.
   - *"Use only this server's … tools…"*: any other cursor movement looks like the user and
     immediately revokes your lease. Use `mouse_move`, `mouse_click`, `mouse_drag`,
     `mouse_scroll`, `type_text` and `press_keys`.
   - In both cases avoid tools that *teleport* the cursor with `SetCursorPos`, such as
     `pyautogui.moveTo` and `pyautogui.click(x, y)` on Windows: they look like the user and cost
     you the lease. Prefer the deskbell tools when unsure.

## Ringing the bell

Call `request_control` with:
- `task`: one short, specific, user-facing sentence, e.g. *"Fill in the shipping form in
  Chrome"*. The user decides based on this, so don't write "GUI automation".
- `agent_name` (optional): who you are if several agents might be running, e.g.
  *"Claude (invoice-repo)"*.
- `wait_seconds` (optional, default 45): how long the call blocks waiting for a decision.

Tell the user in chat that you've rung the bell and what for. Then handle the result:

| Result | What to do |
|---|---|
| `GRANTED` | Start immediately. |
| `WAITING` | Don't touch the mouse or keyboard. Call `request_control` again with the same task to keep waiting; you can do other non-input work between calls, but call at least every 2 minutes or you lose your place. |
| `DENIED` | Stop. Don't ring again for the same task. Continue without the mouse/keyboard or ask the user what they'd prefer. |

## While you have control

- Work briskly. Every action result ends with `Lease: Ns left`. When it gets low (about 15 s)
  and you still have work to do, call `renew_control`.
- **Typing:** click into the target field first, then `type_text` (use `\n` for Enter) or
  `press_keys` for keys and shortcuts (`"enter"`, `"tab"`, `"ctrl+s"`, `"ctrl+shift+t"`).
  Never type when you aren't sure which window has focus: keystrokes in the wrong window can
  do real damage. Be extra careful with destructive shortcuts (`alt+f4`, `ctrl+w`, `delete`).
- Re-take a screenshot after anything that changes the UI before acting on old coordinates.
- Never click the bell widget yourself (it sits in a screen corner, by default bottom-right).
  On Windows it ignores software-generated clicks; elsewhere it can't tell, so this is on you.

## When you lose control

If a tool returns `NOT HOLDING` (revoked, expired or taken back):
1. **Stop at once.** Don't retry the failed action or "finish just this click/word".
2. Assume the user changed things on screen: windows, focus, text, scroll position.
3. If you still need control, call `request_control` again. After a revocation you're put at
   the front of the queue. Take a fresh screenshot once you're granted access again.

## When you're done

Call `release_control` as soon as the last action is finished, and also when you give up or
hit an error, so the next agent doesn't wait for your lease to expire. Then briefly tell the
user what you did.

## Coordinates

`get_screen_info` returns the main display size and cursor position. Coordinates are absolute
from the top-left of the main display: physical pixels on Windows and Linux, logical points on
macOS. Screenshots are often downscaled, so scale screenshot coordinates by
`screen_width / screenshot_width` before clicking.
