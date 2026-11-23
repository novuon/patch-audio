# Patch Remote

## Security update (2026-10-27)

All browsers, including phones over Tailscale HTTPS, must now present the
owner-only code in `~/.config/patch-remote/pairing`. Enter it when prompted;
each browser keeps it in localStorage, so a device pairs once; the server
never returns it. Phone requests must also pass the existing account whois check.
Missing or incorrect codes return 401 without state or a control token.
The static page can load to display the pairing prompt.

TCP loopback does not provide trustworthy SO_PEERCRED process credentials.
A caller can forge Host and X-Forwarded-For using a known account address;
whois confirms ownership of that address, not ownership of the connection.
The pairing secret closes this bypass. The peer-process descriptions in the original preview notes below are
historical: TCP cannot authenticate tailscaled. Pairing is mandatory.
Use the current `tailscale status --json` Self.DNSName in PATCH_REMOTE_ORIGIN
and the dedicated Serve route after renaming the machine.

## Auto Mix first — private HTTPS preview

The current preview is running at:

**`https://<your-host>.ts.net:8124/`**

Connect Tailscale on the iPhone using the same account as this computer, then
open that URL in Safari. The backend permits only the configured account
(`you@example.com` for this preview). That check is Tailscale's local whois
API for the phone's Tailscale address, and only when the TCP connection is
from `tailscaled` itself. A `Tailscale-User-Login` header is not accepted on
its own. Other accounts, Funnel, missing identity, foreign origins and
unpaired sessions are rejected. The Rust listener remains on loopback; the
phone must use Tailscale Serve's private HTTPS endpoint. It has no public
Funnel route and no direct LAN listener. The phone also presents the pairing code.

The root opens Auto Mix. It shows the live playing and upcoming tracks, the engine's
energy-ahead curve, transition plan and rehearsal score. Observe, Suggest,
Prepare and Act control autonomy; Build, Hold and Release set energy direction.
Prepare still requires approval on the desktop. Candidate explanations are
collapsed on phones. These views display engine state rather than scheduling
or reproducing DJ logic in the browser.

The shared palette in `palette.css` matches the active Patch Dark theme:
ink-green backgrounds, paper-grey text and signal red, with cyan and violet
for analysis and upcoming material. It is a bundled palette, not a live theme
subscription. [iPhone capture](screenshots/ai-dj-iphone.png) ·
[Desktop capture](screenshots/ai-dj-desktop.png).

The Booth remains at `/remote/booth`, linked from the header. Mixer is at
`/remote/mixer`; it exposes the existing A/B channel levels, 3-band EQ, filter
and crossfader. Library is at `/remote/library`; search supports Patch's query
syntax, collection filtering, queuing, and loading a selected library track to
a paused deck. Playing decks cannot be replaced. The page returns at most 80
matches per search and leaves playback and AI decisions to Patch.

The Booth page controls the **already loaded A/B decks**: Play/Pause, return to
saved Cue, Sync, channel levels, and the crossfader, with A/Centre/B shortcuts.
It uses Patch's existing `command` API with `origin:user`, including the engine's
normal human-touch/AI handling. It does not duplicate DJ logic or transport.
Cue is an explicit pause and seek to the engine's saved cue point, rather than
a browser-held preview. This avoids leaving a preview playing when Safari sleeps.
The Booth keeps transport controls separate from Mixer. Beat FX and the wider
desktop mixer remain desktop controls for now.

The page has 44px or larger touch controls, phone-safe spacing, and no frontend
framework. State comes from the engine. Each slider coalesces movement into a
bounded latest-value queue, with at most ten dispatched updates per second
across the page and a guaranteed final value while connected. Disconnecting or
backgrounding clears unsent edits; resuming requests fresh state. Commands are
never automatically retried. Rejections restore the confirmed slider state.

Each command carries a random ID. The single IPC worker remembers its latest
128 results, returns a duplicate's original result, and rejects reuse of an ID
for different intent. This protects against recent duplicate delivery, not
indefinite replay storage. A timeout remains an unknown result, since a command
may already have reached the engine. Snapshots establish actual transport state.

## Installed as a boot service

`patch-remote.service` starts at login and runs the remote of the Patch
release that runs: `patch-remote-launch` takes `patch-remote` from the folder
`~/.local/bin/omarchy-patch` points into, or the newest release folder that has
one. Ship `patch-remote` into the same release folder as `omarchy-patch`
(built from the same commit). `patch-remote-follow.path` watches
`~/.local/bin` (a link replaced by rename is not seen on its own path) and
runs `patch-remote-launch --follow`, which restarts the remote only when the
release it should run differs from the one running, so an upgrade carries the
remote along and other changes in that folder do nothing; a failed start is
retried for good. Install or update:

```sh
install -Dm755 remote/patch-remote-launch ~/.local/share/omarchy-patch/patch-remote-launch
install -m644 remote/patch-remote.service remote/patch-remote-follow.path \
  remote/patch-remote-follow.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now patch-remote.service patch-remote-follow.path
```

The Tailscale Serve route (below) lives in `tailscaled`'s own state: the
remote binds 127.0.0.1:8124 and needs no network to start.

## Development preview

The original `patch-remote-preview.service` was a **temporary development service**,
not an installed Patch release. It uses the separately built `patch-remote`
binary and never replaces/restarts the installed audio engine. It survives this
terminal/session but has no boot installation. After editing embedded assets:

```sh
cargo build --release --locked -p patch-remote
systemctl --user restart patch-remote-preview
```

To stop this preview and its dedicated proxy:

```sh
systemctl --user stop patch-remote-preview
tailscale serve --https=8124 off
```

To run interactively after stopping the preview service:

```sh
cargo run --release -p patch-remote -- --control \
  --tailscale-origin https://<your-host>.ts.net:8124 \
  --tailscale-user you@example.com
```

Tailscale Serve is separately configured with
`tailscale serve --bg --https=8124 http://127.0.0.1:8124`.
Use the actual machine's Tailscale DNS name and exact account on other hosts.
Do not overwrite an existing Serve port or enable Funnel. This setup preserves
this computer's pre-existing Serve routes. Do not put another proxy in front
of port 8124.

Serve connects from the local `tailscaled` process and sets `X-Forwarded-For`
to the phone's Tailscale address (CGNAT `100.64.0.0/10` or the Tailscale ULA
`fd7a:115c:a1e0::/48`). Patch checks that peer with `SO_PEERCRED`: the
executable must be `tailscaled`, running as root or as this user. It then
calls the local API `GET /localapi/v0/whois?proto=tcp&addr=<that address>` on
`/var/run/tailscale/tailscaled.sock` (or `PATCH_REMOTE_TAILSCALE_SOCKET` /
`--tailscale-socket`). The request is allowed only when
`UserProfile.LoginName` is `--tailscale-user`. The user running Patch must be
able to open that socket, usually by being in the `tailscale` group. If whois
is unavailable, phone access fails closed. The `Tailscale-User-Login` header
is ignored. Funnel requests are rejected.

Every browser needs the pairing secret
in `~/.config/patch-remote/pairing`, created mode 0600 on first start. The
config directory must not be group or world writable, and a secret that is
not owner-only is refused. The page asks for the code once per device and keeps it in
`localStorage`, sent as `X-Patch-Pairing`. The secret is never included in
a response. `GET /api/session` returns the per-process control token only
after that secret; phone requests must also pass whois. Localhost without the secret
is not a trust domain: another user on the machine can open the page and
still cannot read state, the library, or the token. The pairing secret does
not authorize a tailnet request whose whois login does not match.

Tailscale provides the authenticated private network and HTTPS. A private IP
address, or a copied identity header, does not. See
[Tailscale's local API whois](https://github.com/tailscale/tailscale/blob/main/ipn/localapi/localapi.go)
and [Serve's forwarded source address](https://tailscale.com/docs/features/tailscale-serve).

[Actual iPhone-size browser capture](screenshots/booth-iphone.png)

Validation: Rust tests pass and crate Clippy is clean. Browser checks at
430×932 verified no horizontal overflow, explicit Play/Pause commands, slider
coalescing and delivery of its last value, rejection reconciliation, disabling
controls on disconnect, and fresh state on resume, with no JavaScript errors.
All browser command tests used intercepted responses; no test changed the live
music. The real HTTPS route/session/state and rejection checks passed. Actual
physical iPhone touch input remains for hands-on use.

## Other development surfaces

Run from the repository root:

```sh
cargo run -p patch-remote
```

Open `http://127.0.0.1:8124/remote/dj` or
`http://127.0.0.1:8124/remote/visualiser`. The page asks for the pairing
secret printed as a path at startup (`~/.config/patch-remote/pairing`). The
default is read only. To enable local control, restart with
`cargo run -p patch-remote -- --control`. The installed Patch engine must
already be running; this binary never starts, upgrades, installs, or restarts
it. No external frontend packages are needed. Pages are embedded at compile
time, so rebuild after editing `remote/`.

## What is real

The Auto Mix renders `patch_core::aivis::VisualState` from `ai_state`: sources,
transport positions, mode, phase, direction, ranked candidates, reasons, and
transition execution. Controls use existing `ai_mode` and `automix.direction`
commands. “Step back to Suggest” changes autonomy; it does not promise to cancel
an already executing transition. “Mix now” uses the human `dj.transition_now` command: an existing AI plan
is moved to the next bar; without a plan the engine prepares the transition.
It does not perform an immediate cut. “Change next” uses `dj.session` to reject
the selected next track and request another. Both buttons are near the top.
Approval is not exposed yet; approval should gain a backend pending-plan identity before it
becomes a reliable action across delayed or duplicated browser requests.

The Visualiser reads `visual_intent.get`, and writes partial semantic intent
through `visual_intent.set`. Intensity, motion, density, symmetry, and feedback
are real intent controls. “Return to director” clears the override. These
controls affect an existing renderer that consumes intent; the intent API
does not establish that a Visualiser window or output is running. Scene,
mode, hold, and blackout controls remain unavailable because they currently
belong to the desktop surface. The page now includes a lightweight Canvas preview
of semantic intent, with theme colours and a beat estimated between deck-state
snapshots. This is not desktop output capture or audio spectrum analysis.
When no posted intent is active the preview uses the contract defaults; the
actual director's evolving render state is not reported by this API.
Colour, focus, section and transition duration are validated, bounded partial
intent controls. Quick looks send a single merged intent. Demo mode and its
controls stay local and work without the engine. Pause and full screen affect
only the preview. Animation is capped at 30 fps, pixel density at 1.5, and it
stops while hidden; reduced motion starts paused.

`visual.js` owns controls and network state; `visual-preview.js` owns drawing.
No audio data or rendering enters the realtime engine from the browser.

There is no mock data. Disconnected displays may retain their last values,
but show the disconnection and disable controls. Matte-black colours, accent,
monospace typography, borders, and compact controls follow
`crates/patch-ui/ui/theme.slint`. The bundled palette matches Patch Dark, not a live theme feed.
Desktop uses paired columns; phone uses one column and 44px controls. Browser
captures in `screenshots/` show actual running engine state in read-only mode.
Mobile captures validate responsive viewport layouts, not physical touch devices.

## Separation and bounds

- `crates/patch-remote/src/access.rs`: pairing secret and whois allowlist. `identity.rs` checks the TCP peer and calls Tailscale's local API.
- `crates/patch-remote/src/http.rs`: fixed embedded routes, HTTP validation,
  same-origin session protection, four workers and a 32-connection queue.
- `crates/patch-remote/src/engine.rs`: one IPC worker, one kept Unix connection,
  a 16-command queue, and a shared snapshot. No engine/DSP dependency.
- `remote/client.js`: renders state and submits intent. Sends sliders on release,
  preserves source DOM nodes, and updates candidates only when their data changes.
- `remote/booth.{html,js,css}`: the phone Booth, coalesced control delivery, and presentation.
- `remote/mixer.{html,js,css}`: A/B levels, EQ, filters and crossfader.
- `remote/library.{html,js,css}`: library search, collection filtering, queue and safe deck load.
- `visual.{js,css}` and `visual-preview.js`: visual intent controls and the isolated browser preview.

Engine polling runs at four snapshots a second regardless of viewer count, plus
an immediate refresh after commands. AI/Visualiser browsers read the cache twice
a second; Booth reads four times a second. All pause while hidden. Engine replies
and HTTP input have size/time limits. Expired queued commands are discarded
before dispatch. Commands are never retried automatically: a timeout may mean
execution already happened. Controls remain disabled during a request. The
engine owns state and timing; no browser timer schedules a mix. Reconnection
fetches a new session and authoritative snapshot.

Commands are limited to explicit intent translations. Auto Mix and Visualiser
writes are idempotent settings, with last dispatch winning. Visual edits send only the
changed field, so changing motion does not overwrite another controller's
intensity. There is no exclusive control ownership or atomic compare-and-set
contract yet. Acknowledgement means the engine accepted the request; subsequent
snapshots establish the displayed state and transition progress.

## Default local access boundary

This is a development service, not a production LAN server. It binds only
`127.0.0.1:8124`, accepts only that exact Host or the configured Tailscale
Host, and rejects foreign Origin headers. Responses forbid framing and
external scripts through CSP. By itself it provides no TLS and no LAN
listener.

Localhost is not a trust domain. The static pages load so a browser on this
computer can prompt for the pairing secret; `/api/session`, `/api/state`,
`/api/library` and `/api/command` do not. Commands still need the per-process
session token, the exact Origin, and `--control`. The phone profile above is
the other way in: Tailscale Serve for HTTPS, `tailscaled` as the TCP peer,
and whois for the account. Do not place an unrestricted forwarding proxy in
front of it. The pairing file is mode 0600, the same idea as the engine
socket: other users on the machine cannot read it.

## Validation and measurement

```sh
cargo test -p patch-remote
cargo clippy -p patch-remote --all-targets -- -D warnings
node --check remote/client.js
```

Rust tests cover the intent allowlist, fragmented IPC/rejections, Host/Origin
and read-only protection, authenticated command acknowledgement, the pairing
file mode, refusal to hand the control token to an anonymous local client,
whois-versus-header allowlisting, duplicate command handling, human command
validation, and safe return-to-cue composition through a mock transport.
Control tests never send mutations to the user's engine.

On 2026-10-11, 100 sequential cached `/api/state` requests through Python
urllib on this machine measured median **0.131 ms**, p95 **0.381 ms**, max
**0.589 ms**. This measures localhost HTTP cache serving only, not musical
execution, Wi-Fi delay, browser display latency, or audio deadline impact.

Next: introduce backend identities for prepared-plan approval, expose a narrow
Visualiser scene/output contract, then measure actual phone/tablet use with a
small trusted HTTPS arrangement. Every browser authenticates with the pairing secret; phone access additionally
requires the Tailscale whois account match.

## Navigation for every remote page

All pages use the same `patch-navigation` component from `navigation.js` and
`navigation.css`. Include both assets and place
`<patch-navigation page="dj"></patch-navigation>` before the main content.
Use the page's ID for its active tab. When adding a page such as Mixer, add its
ID, label, title and URL to the single `pages` list in `navigation.js`, and add
its embedded route in `patch-remote`. Every page then receives the new link.
Keep connection status in the page content, outside the shared navigation.

The Mixer Unit page replaces the DJ deck Mix page at `/remote/mixer`. It
presents the engine's live channel list, bounded per-channel gain and mute
controls, and the Patch master output. `/remote/unit` is an alias. Remote
intents use channel identifiers returned by the engine status snapshot.

Auto Mix's top countdown uses the scheduled start beat minus the outgoing deck's
reported beat. Bars assume four beats; minutes/seconds use that deck's effective
BPM. It refreshes with engine snapshots, follows rescheduling, marks paused
playback, and clears on disconnection or once the transition is running.

## Choreography intent experiment

The desktop visualiser now polls posted intent in Choreography as well as
Hybrid. `patch-visual::steering` smooths numeric amounts over transition beats,
scales the visual motion clock and backdrop brightness, adjusts backdrop density
and mirroring, adds bounded fading cell trails, applies colour behaviour and
selects existing bass/mid/high analysis for focus. Text is composed afterward
for readability. Section and intensity steer the director's calm/groove/lift
choice at its existing phrase boundaries. Arrivals and actual DJ handovers keep
priority. This steers existing choreography; it is not manual scene selection.
Clearing intent restores the director's clocks, palette and pattern decisions.

A new desktop window publishes a small atomic `visualiser-status.json` heartbeat
once per second under the Patch runtime directory. The remote worker reads a
bounded 4 KiB document and discards reports over three seconds old; the page
shows mode, held state and whether intent is applied. An older desktop build
has no heartbeat and is reported as unavailable. The installed desktop must
receive a committed release of this bridge before remote edits affect it.

## Installed private service

The committed release installs `patch-remote` alongside `omarchy-patch` in the
release directory, with `~/.local/bin/patch-remote` pointing to that copy.
`remote/patch-remote.service` is the user-service template. It requires
`~/.config/patch-remote/environment` with `PATCH_REMOTE_ORIGIN` (the exact private
HTTPS endpoint) and `PATCH_REMOTE_USER` (the Tailscale login whois must
return). The first start also writes `~/.config/patch-remote/pairing` mode
0600. The installed service is `patch-remote.service`; restart or stop it with
`systemctl --user restart patch-remote` or `systemctl --user stop patch-remote`.
Tailscale Serve keeps the dedicated HTTPS route on port 8124. The listener
remains loopback. Phone requests are allowed only when `tailscaled` is the
peer and whois names `PATCH_REMOTE_USER`. The user must be able to open
`tailscaled.sock`.
