# Getting a controller working in Patch

A walkthrough from "I own a controller" to "its pads move a deck", in the order
the pieces exist. Every command here has been run, and every output shape shown is
what it actually prints.

**The honest caveat first.** No physical controller has been exercised while this
capability was built: the device paths are FIFO-backed simulations and unit tests.
So this describes what Patch *will* do, not what it has been seen doing on your
desk. Treat the first device as a test, and expect the report to tell you more than
you expected.

## 1. Plug it in and look at the Controllers unit

Open the unit (`omarchy-patch open controllers`, or its `h` binding). Devices are
checked every 2 s, and a profile that matches loads itself — there is nothing to
press.

You will see one row per *device*, not per interface: a controller that presents
several endpoints is one row. Each row carries the transport, the device's name,
whether a profile is attached (`profile` / `no profile`), the path, its state and
how strong its identity is — and then how the profile was chosen, with what it
would do. That last line is built as:

    <how the profile was chosen> · <N> bindings · <M> outputs
    · <sent> sent · <dropped> dropped

(the counts and the traffic appear only where they mean something). Read that
second half before believing a controller works: `matched` alone says a profile was
chosen, not that it does anything.

Three outcomes are worth knowing by sight:

* **`no profile matches`** — the device is seen and nothing drives it yet. Go to
  step 3.
* **`matched by …`** — a profile drives it. Read the counts on the next line.
* **`N profiles match by … (…, …) · choose one`** — two profiles claim the device
  and Patch refuses to guess. Remove or disambiguate one by hand.

A keyboard or a mouse is not a controller, whatever it presents, and stays on the
key-binding path.

## 2. Know which kind of device you have

* **MIDI** — the common case, and the one this workflow serves.
* **HID** — Patch lists it but cannot read it yet. Every HID preset in Mixxx's
  corpus is driven by JavaScript and carries **zero** declarative controls, so
  nothing can be imported for it. An imported HID preset will match your device and
  report `0 bindings · nothing will respond`. That is the truth, not a broken
  import: see `docs/controllers-scripting.md` for what would change it.
* **USB bulk** — out of scope, and the importer refuses such a preset by name.

## 3. Get a mapping to import

Mixxx's corpus is the source: `res/controllers/*.midi.xml` in
[mixxxdj/mixxx](https://github.com/mixxxdj/mixxx). Patch ships no mappings and
copies none — you import the file yourself, whatever its licence, because the
result is a description of your hardware in Patch's own vocabulary, not a copy of
anyone's code.

Pick the file matching your model. The `id` in the file names the MIDI port, so a
wrong pick is usually visible as a criterion that will never match (step 5).

## 4. Import it

    omarchy-patch controllers import /path/to/Your_Controller.midi.xml

That prints a report and then the profile, and writes nothing:

    N controls: 71 translated, 98 script-bound, 111 without a Patch equivalent;
    M outputs: 42 translated, 108 without a Patch equivalent; …
      not translated: LoadSelectedTrack (group [Channel1]) x1 · this names the DJ
        Booth's library *selection*, which a controller cannot see …
      note: imported profiles are a starting point: no physical controller has
        been used to check any converted mapping

**Read the refusals.** They are the most useful part of the import: each one names
the control object and says why it has no equivalent — a Mixxx effect parameter,
a sampler, the library selection, a normalised knob with no stated curve. Two
thirds of Mixxx's entries are JavaScript and cannot be translated at all; those are
counted separately rather than reported one by one.

When the report looks right, write it:

    omarchy-patch controllers import /path/to/Your_Controller.midi.xml --write

    wrote ~/.config/omarchy-patch/controllers.json · 1 profiles · reload with:
    omarchy-patch controllers reload

The file is validated before it is written, an existing profile with the same id is
refused rather than duplicated, and the write is staged and renamed so a failure
cannot leave a half-written profile list. Then:

    omarchy-patch controllers reload

## 5. Check that the profile will actually match

This is the step that quietly decides whether anything works. A profile matches on
the criteria in its `"match"` block, and each criterion must appear *inside* what
Linux reports for the device:

* **`vendor_id` / `product_id`** — from the device itself, the strongest criterion
  short of a serial.
* **`names`** — a case-insensitive substring. For a MIDI preset this is the
  mapping's own `<controller id>` (the port name, which is exactly what a device
  reports). For a HID preset, which has no `<controller>` element, the preset's own
  name is used — and because that carries the vendor (`Native Instruments …`) and a
  device does not, the file's own name is added as a second criterion too.
* **`serial`** — the strongest of all, and the only way to tell two identical models
  apart.
* **`card_id`**, **`event`**, **`interface`** — the ALSA card's id, an evdev node, or
  a USB interface number.

A device node path appears in the unit's row but is **not** a criterion: pin a
profile with a vendor/product id or a serial, and the profile keeps matching when
the device is replugged into a different socket.

If Patch says `no profile matches`, compare the row's identity against the profile's
criteria and add a `vendor_id`/`product_id` from the row. An imported profile that
carries only the mapping's own name is the usual cause: the name criterion is a
substring test, and a name Mixxx took from a preset is not always what Linux calls
the device.

## 6. Try it, and read what the unit says

Press something. The unit's event list shows each message and either the command it
ran or, unbound, the trigger to paste into a binding — which is how a missing
control is added by hand. The row's `sent` / `dropped` counters show feedback going
back to the device; `dropped` climbing means the device's buffer is full, not that
Patch is behind.

## 7. Editing by hand

A profile is JSON you can edit: `bindings` map device messages to canonical
commands, `outputs` drive LEDs from Patch's own state, and `soft_takeover` avoids a
knob jumping when a profile reconnects. The vocabulary is in `docs/controllers.md`.

After any edit: `omarchy-patch controllers reload`. `omarchy-patch controllers
status` prints the running service's own view as JSON — devices, profiles, events —
which is what to paste when reporting a problem.

## 8. When it does not work

* **The device is not listed.** For MIDI, check `amidi -l` and `aconnect -l`: the
  port must exist before Patch can open it. For HID, check `ls /dev/hidraw*`.
* **Listed, no profile matches.** Step 5 — the criterion, almost always.
* **Two profiles match.** Remove one; Patch will not choose.
* **Matched with 0 bindings.** The mapping was script-bound. Nothing to bind.
* **An LED is wrong or dark.** The output's `source` may name state Patch does not
  have, or the target's byte may be wrong for your model. The feedback path is
  documented in `docs/controllers.md` under the outputs table.
* **A key or pad does nothing** while the unit shows the message: the control object
  is refused. Re-run the import and read the reason, or bind it by hand.
* **"cannot open · /dev/hidraw6: Permission denied"**, or a device that stays
  "detected · HID device Patch cannot read yet" while a profile matches it. A
  vendor-protocol controller's `/dev/hidraw` node is `root:root`. It has no evdev
  node in the seat, so logind's uaccess tag never reaches it and your user cannot
  read it. Install the rule Patch ships, then replug the device (§10).
* **"Profile error · HID devices must be paths under /dev/input"**, in the unit
  and in the title bar. This build refuses the form your config uses for a raw
  device; §10 has the forms it accepts. The banner is a whole-file rejection, so
  every profile in the file is ignored while it is up — including ones that would
  otherwise match.

## 9. What is already known to be missing

Effects (Mixxx effect units, parameters, enables), samplers, beat sync, the
library selection, `pregain` and master volume (normalised knobs with no stated
curve), and the whole scripting tier. Each is sized and explained in
`docs/controllers.md` under *Not implemented*, with the measurement behind it.

## 10. Controllers that speak their own protocol

Some controllers — the Traktor Kontrol family, and most DJ hardware — expose no
MIDI port and do not decode into evdev either. The kernel reads HID for its own
input subsystem, and this device is vendor usage, so nothing but the device's
own driver can read it. Patch reads `/dev/hidraw*` itself for these. Three
things follow, and all three are needed.

**Grant access, once.** The node is `root:root`, and unlike a keyboard or a mouse
it never gets the seat's uaccess tag, because it exposes no evdev node in the
seat. Patch ships the rule; the installer installs it, and by hand it is:

```sh
sudo install -Dm644 packaging/70-omarchy-patch-controllers.rules \
  /etc/udev/rules.d/70-omarchy-patch-controllers.rules
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw
```

Replug the device afterwards. Until this is done the unit shows the node and
says why it cannot open it — which is the honest answer, not a failure to
detect it.

**Name the device.** HID is opt-in: a raw device is opened only when the user
names it in `hid_devices`. Either the node (`/dev/hidraw6`, which moves when
anything else is plugged in) or, better, the identity the unit reports for it
(`usb:17cc:1101`), which survives a reconnect:

```json
{
  "hid_devices": ["usb:17cc:1101"],
  "profiles": [ { "id": "…", "name": "…", "protocol": "hid", "keymap": { "bindings": [ … ] } } ]
}
```

**Declare the fields.** A raw device opens only when a profile for its identity
binds report fields: a report id, an offset, a width and a mask. Those bindings
*are* the field table — the transport reads exactly what something listens to,
and nothing else — so a profile with no fields leaves the device listed rather
than opening it into silence. A field a value-window makes a press (a platter
with no touch bit) is declared as such, and the device's own calibration is read
from its feature reports when it offers them.

Capturing a field table beats guessing one: press the control and the unit's
Monitor prints the trigger JSON to paste into a binding, so the offsets come off
the hardware. `docs/controllers-traktor-s2-mk1.md` is a worked example of the
whole path.
