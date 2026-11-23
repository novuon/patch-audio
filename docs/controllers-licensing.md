# Controllers: licensing and intellectual property

This note analyses what Patch can and cannot reuse from Mixxx's controller
infrastructure, records what was verified, and flags where the answer depends on
facts not resolvable from the repository. It is not legal advice; anything that
binds Patch commercially should be reviewed by counsel.

## Facts established

- Mixxx top-level `LICENSE`: "Mixxx 2.7-alpha ... GNU General Public License
  ... version 2 ... or (at your option) any later version". Its only carve-out
  is for **skins** ("licensed under a Creative Commons license ... located in
  their skin.xml file"). There is **no per-directory licence section for
  `res/controllers`**.
- Patch `LICENSE`: MIT, Copyright (c) 2026 novuon.
- Patch `NOTICE.md`: Patch's own source is MIT. It states that a **binary** as
  built today links Rubber Band and FFTW (GPL-2.0-or-later) and Slint
  (GPL-3.0-only or the Slint licence), so the distributed Arch package is
  "GPL-3.0 as a whole; its source stays MIT".
- The 164 controller mapping XMLs at Mixxx commit
  `44dc9ed7e8640b1d69b2d63f847d940b5ad688b1` (144 MIDI, 18 HID, 2 bulk).
- The repository tree lists **152 JavaScript controller scripts, all under
  `res/controllers/`**, plus ~296 QML files under `res/controllers/`.

Counts from the local corpus (verified by grep over all 164 XML files):

- **163 of 164 XML files contain no copyright, licence or permission notice of
  any kind.** No file contains an SPDX identifier.
- **One file carries a notice: `Electrix_Tweaker.midi.xml`**, a GPL v2-or-later
  block ("Electrix Tweaker controller script 0.2 for Mixxx 1.12", Copyright
  (C) 2015 Be).
- The four `Pioneer-DDJ-*.midi.xml` files matched a case-insensitive "gpl"
  search, but only as a substring of the control name `jogPlatterTick`; they
  carry no notice.
- **163 of 164 files carry a `<author>` element**, spanning 118 distinct author
  strings (one empty), so metadata is present even where a licence notice is not.

Reporting the absence of per-file notices is a finding, not a fault. Copyright
in a mapping file belongs to its listed `<author>`; the file is nonetheless
*conveyed* by Mixxx under GPL-2.0-or-later, because the repository's
distribution terms cover what the project ships.

## 1. Licences in play

- **Patch**: MIT (source), with the binary caveat recorded in `NOTICE.md`
  above.
- **Mixxx**: GPL-2.0-or-later. Copyright in a mapping file belongs to its
  `<author>`; copyright in the Mixxx source tree as a whole belongs to the
  Mixxx Development Team and contributors. Mixxx conveys the mapping files
  under GPL-2.0-or-later. The mapping file is a *copyrighted work* — a
  human-readable expression of control assignments — not a bare fact, even
  when it contains nothing but numbers.

## 2. Why copying Mixxx C++ is not possible

GPL-2.0 and MIT are incompatible in the direction Patch needs. An MIT project
cannot absorb GPL-2.0 code and keep distributing the result under MIT alone:
GPL-2.0 requires that distributed derivative works be licensed under the GPL
and that source be offered to recipients. Adding GPL-2.0 code to Patch would
therefore pull the affected files (and the combined work) out of MIT.

GPL-3.0 code is likewise incompatible with keeping Patch's source MIT — it
cannot be relicensed to MIT — even though GPL-3.0 is compatible *upstream* with
GPL-2.0-or-later.

Two honest qualifications:

- The **verb** matters as much as the direction. Reading a file, learning from
  it, and independently implementing the behaviour are not "copying". Copying
  source, translating it closely (for example a C++ function rewritten in Rust
  line-by-line), or linking against it are the acts that trigger GPL
  obligations for a derivative or combined work.
- Patch's own `NOTICE.md` states the **shipped binary** is already a combined
  work distributed as GPL-3.0 as a whole, because of Rubber Band/FFTW and
  Slint. That is about the binary's distribution terms, not about making
  GPL-2.0 code safe to place in Patch's **source**. If Patch's intent is to
  keep its source MIT and free of GPL obligations, source-level absorption of
  Mixxx code remains off the table. See section 8.

## 3. Mapping XML files

The finding: of 164 files, 163 carry no per-file notice; one
(`Electrix_Tweaker.midi.xml`) is explicitly GPL-2.0-or-later. The repository has
no `res/controllers` licence section, so the default remains repo-wide
GPL-2.0-or-later. The following activities are distinct and must be reasoned
about separately. For each: do GPL obligations attach to Patch, and why.

**(a) Reading a mapping file at runtime that the user supplies themselves.**
Patch reads a file from the user's own disk, chosen by the user, and does not
redistribute it. Reading a file does not create a derivative work of it, and no
copy of it is conveyed by Patch. GPL obligations to third parties do not attach
to Patch on this basis. The user's own use of their own file is between them and
the file's author. Fact-dependent: if Patch *ships* the file, or fetches it and
redistributes it, this stops being true.

**(b) Converting such a file locally, at the user's request.** This runs on the
user's machine, over the user's file, and the output stays with the user.
Private use, on the same reasoning as (a), does not impose GPL distribution
obligations on Patch's source. The output is a derivative of the user's input,
and the user — not Patch — is the one who holds and uses it. Whether the
*converter* must be GPL is the crux: a tool that reads a format and emits a
different format is not itself a derivative of the files it reads. Fact-dependent
and worth review.

**(c) Bundling converted mappings inside an MIT Patch release.** The first
activity that plainly triggers GPL obligations. Shipping converted or adapted
copies of GPL files means Patch conveys derivative works of GPL-2.0-or-later
material; those files must be GPL, with source and notices, and cannot be
presented as MIT. This does not "infect" unrelated Patch source, but it
contaminates the bundled data files and any claim that the package is MIT.
Do not do this unless the project is willing to carry GPL terms on those files.

**(d) Bundling the original XML files unchanged.** Verbatim redistribution of
GPL-2.0-or-later files is permitted but must comply with the GPL: keep the
files' notices and the licence text, and state the terms. This is not MIT
material. It is *lawful* to ship them under GPL terms alongside an MIT program
(aggregation), but it means the release contains third-party GPL files and the
packaging must say so. It also pulls in the scripts the XMLs reference (section
4). **Simplest position: do not bundle them.**

**(e) Documenting the file format and writing an independent reader.**
Documenting a format — the schema, element and attribute names, and the
semantics of options such as `soft-takeover`, `fourteen-bit-msb`/`-lsb`,
`script-binding`, `invert` — is describing an interface. An independent reader
that parses that format implements an interface; it is not a copy of any
mapping file nor of Mixxx's reader source. Writing such a reader from the
documented format, from observed files, and from the option semantics is a
normal interoperability activity and does not by itself make Patch GPL.
Fact-dependent caveat: if the reader is written by translating Mixxx's own C++
parser closely, it becomes a derivative of that source and GPL applies.

**(f) Independently implementing a hardware protocol after observing a
device's behaviour.** Observing how a device you own behaves and writing your
own code to talk to it is independent implementation. The resulting code is
your expression; it is not a derivative of Mixxx or of a mapping file. This is
the activity least likely to attract GPL obligations, provided the
implementation is genuinely independent rather than a transcription of
existing source. Fact-dependent: a specific author may have licensed their
contribution differently to what the repository default implies, and a
transcription of one of their files is a derivative of *their* expression
regardless.

## 4. JavaScript controller scripts

Mixxx ships 152 controller scripts, all under `res/controllers/`, and its
engine executes them against its control objects. What was verified here: the
**file list** (152 JS files, all under `res/controllers/`) and the XML corpus
above. The **contents** of the individual `.js` files were not re-fetched in
this pass, so treat the "no per-file notices" finding as confirmed for the XML
corpus and pending for the JS corpus. What is known: the one XML carrying a GPL
notice is the wrapper for `Electrix-Tweaker-scripts.js`, and its notice
describes the script itself, so at least one script is explicitly
GPL-2.0-or-later. Check the rest before drawing a conclusion.

Two distinct activities:

- **Executing original Mixxx scripts inside Patch's own JS engine.** Running a
  script is not conveying a copy; if the script arrives on the user's machine
  and is executed there, the private-use reasoning of section 3(a)/(b) applies.
  The relevant question is the *engine*: Patch's JS engine must be its own
  work, not a copy of Mixxx's scripting host. Providing a compatible scripting
  API surface (the names and semantics of the objects a script expects) is
  interoperability, not copying, to the same extent as section 3(e). Patch's
  sidecar model (a Node process, per `docs/third-party-engines.md`) is
  separate from Mixxx's C++ host.
- **Translating scripts into another form.** A translation is a derivative work
  of the script it translates. Producing such a translation *for the user, on
  the user's own file, locally* does not convey a copy and does not attract GPL
  obligations to Patch; *bundling* translated scripts, or shipping translated
  versions of Mixxx's scripts, does (section 3(c)).

Because the JS files' own notices were not verified here, treat the "no
per-file notices" claim as true for the XML corpus and pending for the JS
corpus.

## 5. What an MIT Patch may safely ship

- An **independent mapping format** designed by Patch (see `controllers.md`,
  the JSON profile format) and a reader for it.
- An **importer** that runs on files the user supplies, converting in place,
  writing the result to the user's own configuration directory.
- A **documented compatibility subset** — element/attribute/option names and
  their semantics — so users and contributors can map files by hand.
- **No bundled Mixxx XML, JS, QML or C++.** Nothing from `res/controllers` and
  nothing from Mixxx's source tree goes into a Patch release as a shipped
  artifact. Reference the format in prose; do not ship the files.
- If any exception is ever made (for example shipping one file), it must be
  under GPL terms with its notice and the licence text, and the release notes
  must say so. The clean default is none.

## 6. Protocol and hardware knowledge

Functional and interface information needed for interoperability — MIDI note
and CC assignments, SysEx sequences, USB vendor/product IDs and endpoint
descriptors as used to talk to a device the user owns — sits differently from
copyrightable expression. Copyright protects the particular expression (a file,
a source listing); it does not, in the usual understanding, protect the
underlying interface facts. The distinction that matters:

- **Learning from documented or observed behaviour** and then **independently
  implementing** the interoperability behaviour is the standard basis on which
  independent controller implementations exist. Documentation and observation
  are inputs to your own expression.
- **Copying source** — a file, a function, a script — is taking the expression,
  and that is what carries GPL obligations.

A reasonable working position is that Patch may implement device support from
documented behaviour, from observing devices the user owns, and from a format
Patch documents itself. Where a particular fact is uncertain — for example a
specific mapping's provenance, or whether a device protocol is affected by a
party's separate terms of use — treat it as needing review rather than
asserting a conclusion.

## 7. Attribution

When a user converts their own file, preserve the original metadata without
implying Patch bundles or endorses anything:

- Carry over the source `<author>`, `<name>` and `<description>` from the
  original file into the converted output, marked as provenance. For example,
  record `source_author`, `source_name`, `source_description` and the source
  filename/commit, distinct from the converted file's own authorship.
- Do not strip the original's notices. If the source file carried a licence
  notice (as `Electrix_Tweaker.midi.xml` does), the converted copy should retain
  it and the user should be told the converted file inherits the source's
  terms. A converted derivative of a GPL file is not automatically MIT.
- A NOTICE-style record for a converted artifact should contain: the source
  file's name and path; the source project and commit
  (`mixxx@44dc9ed7e8640b1d69b2d63f847d940b5ad688b1`); the source `<author>`;
  the source licence if one was present; a statement that the file was
  converted by the user on their own system and that Patch did not author,
  bundle or endorse the original. Where no per-file notice existed, say so
  rather than inventing a licence.

## 8. Open questions / needs review

- **JS script notices**: the contents of the 152 `res/controllers/*.js` files
  were not inspected in this pass. Confirm whether any carry notices beyond
  Electrix Tweaker's, before relying on the "no per-file notices" finding for
  scripts.
- **The binary licensing question**: `NOTICE.md` says the shipped binary is
  GPL-3.0-as-a-whole. How that interacts with keeping the source MIT and with
  any bundled controller data needs an explicit decision, because it is easy to
  conflate "the binary's terms" with "what source Patch may contain".
- **Per-author licensing**: the repository default is GPL-2.0-or-later, but a
  contributor could in principle have licensed their file differently. There is
  no per-file notice to check against, so this cannot be resolved from the
  corpus.
- **Importer boundary**: confirm the importer reads files selected by the user
  and never fetches or redistributes them, and that the converter is not a
  close translation of Mixxx's parser.
- **Compatibility subset**: define in code which options
  (`soft-takeover`, `fourteen-bit-msb`/`-lsb`, `script-binding`, etc.) Patch
  supports, and document them as a format Patch implements, not as a copy.
