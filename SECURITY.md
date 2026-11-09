# Security policy

deskbell sits between AI agents and your physical mouse and keyboard, and it updates itself, so
security reports are very welcome.

## Reporting a vulnerability

Please report privately through GitHub: **Security** tab → **Report a vulnerability**
([direct link](https://github.com/H3katonkheir/deskbell/security/advisories/new)). Don't open a
public issue for anything exploitable.

Include what an attacker needs (e.g. "a web page the user visits", "an agent holding the
lease", "write access to the release"), what they gain, and steps to reproduce. You should hear
back within a week. Fixes ship in a new release, and the advisory is published once installed
copies can update to it.

Only the [latest release](https://github.com/H3katonkheir/deskbell/releases/latest) is
supported.

## In scope

- **Self-update:** installing or running code that isn't a release signed with the maintainer's
  key, or moving an installed copy to an older version.
- **The local HTTP endpoint** (`127.0.0.1:47321`): any way for a web page or another machine to
  reach it.
- **The bell's own protections** on Windows with *Only my physical mouse and keyboard count*:
  software-generated input approving a request, closing the bell, turning on auto-fire or
  changing settings, or an agent keeping control after the user moves the mouse.
- **Install, uninstall and start-at-login:** writing outside deskbell's own files and entries, or
  running something other than the installed binary.
- **Crashes or hangs** that a request to the MCP endpoint can trigger.

## Known limitations (not vulnerabilities)

These are described in the README under *Things to be aware of*:

- deskbell is a courtesy protocol, not a sandbox. An agent with its own input tools, file
  access or shell can ignore the bell, edit the config file or end the process.
- Any local process can talk to the HTTP endpoint.
- On macOS and Linux, and with *Any movement* on Windows, software-generated clicks can't be
  told apart from the user's.
- Software that injects input for the user (mouse sharing, remote control, voice control) looks
  like an agent to the bell.
- Agents choose the name and task shown in the speech bubble.
