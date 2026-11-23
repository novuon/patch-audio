use patch_surfaces::player::carts::{self, Board, Do};
use patch_surfaces::player::{self, Action};

const CART_USAGE: &str = "player cart <pad> | cart <bank> <pad> | cart stop [<pad>] | cart fade [<pad>] | cart bank <bank> | cart set <pad> <file> [name] | cart option <pad> loop|fade|hold|duck on|off | cart colour <pad> <0-5> | cart clear <pad> | cart status   (pads 1-12, banks 1-4 or a name)";

/// A pad by its number (1 to 12) or its key in the window (0, -, =).
fn pad_index(word: &str) -> anyhow::Result<usize> {
    if let Some(i) = carts::KEYS.iter().position(|k| *k == word) {
        return Ok(i);
    }
    match word.parse::<usize>() {
        Ok(n) if (1..=carts::PADS).contains(&n) => Ok(n - 1),
        _ => anyhow::bail!("no pad '{word}': use 1 to {}", carts::PADS),
    }
}

/// A bank by its number (1 to 4) or the start of its name.
fn bank_index(board: &Board, word: &str) -> anyhow::Result<usize> {
    if let Ok(n) = word.parse::<usize>() {
        anyhow::ensure!((1..=carts::BANKS).contains(&n), "no bank {n}: use 1 to {}", carts::BANKS);
        return Ok(n - 1);
    }
    let word = word.to_lowercase();
    board
        .banks
        .iter()
        .position(|b| b.name.to_lowercase().starts_with(&word))
        .ok_or_else(|| anyhow::anyhow!("no bank named '{word}'"))
}

/// The carts from a script or a desktop key binding: the same board and
/// voices as the window's pads.
fn cart(words: &[&str]) -> anyhow::Result<()> {
    let mut board = Board::load();
    let trigger = |board: &Board, bank: usize, index: usize| Do::Trigger {
        bank,
        index,
        pad: board.pad(bank, index).cloned().unwrap_or_default(),
        solo: board.solo,
    };
    let action = match words {
        [] | ["status"] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"board": board, "playing": carts::status()}))?
            );
            return Ok(());
        }
        ["stop"] => Do::StopAll { fade: false },
        ["fade"] => Do::StopAll { fade: true },
        [verb @ ("stop" | "fade"), pad] => Do::Stop { bank: board.bank, index: pad_index(pad)?, fade: *verb == "fade" },
        ["bank", bank] => {
            board.bank = bank_index(&board, bank)?;
            return board.save().map_err(anyhow::Error::msg);
        }
        ["set", pad, file] | ["set", pad, file, _] => {
            let index = pad_index(pad)?;
            let path = std::path::Path::new(file);
            let file = if file.contains("://") {
                file.to_string()
            } else {
                anyhow::ensure!(path.is_file(), "no such file: {file}");
                std::fs::canonicalize(path)?.to_string_lossy().into_owned()
            };
            let bank = board.bank;
            board.place(bank, index, &[(file.clone(), carts::seconds_of(&file))]);
            if let [_, _, _, name] = words {
                board.banks[bank].pads[index].label = name.to_string();
            }
            return board.save().map_err(anyhow::Error::msg);
        }
        ["option", pad, what @ ("loop" | "fade" | "hold" | "duck"), on @ ("on" | "off")] => {
            let (bank, index, on) = (board.bank, pad_index(pad)?, *on == "on");
            let pad = &mut board.banks[bank].pads[index];
            match *what {
                "loop" => pad.looped = on,
                "fade" => pad.fade = on,
                "hold" => pad.hold = on,
                _ => pad.duck = on,
            }
            return board.save().map_err(anyhow::Error::msg);
        }
        ["colour" | "color", pad, colour] => {
            let (bank, index) = (board.bank, pad_index(pad)?);
            let colour: u8 = colour.parse()?;
            anyhow::ensure!(colour <= carts::COLOURS, "colours are 0 (none) to {}", carts::COLOURS);
            board.banks[bank].pads[index].colour = colour;
            return board.save().map_err(anyhow::Error::msg);
        }
        ["clear", pad] => {
            let (bank, index) = (board.bank, pad_index(pad)?);
            carts::run(Do::Stop { bank, index, fade: false }).map_err(anyhow::Error::msg)?;
            board.banks[bank].pads[index] = Default::default();
            return board.save().map_err(anyhow::Error::msg);
        }
        [pad] => trigger(&board, board.bank, pad_index(pad)?),
        [bank, pad] => trigger(&board, bank_index(&board, bank)?, pad_index(pad)?),
        _ => anyhow::bail!("{CART_USAGE}"),
    };
    // A fade is this process's work: it ends when the fade has.
    carts::run_and_wait(action).map_err(anyhow::Error::msg)
}

pub fn run(words: &[&str]) -> anyhow::Result<()> {
    let number = |s: &str| -> anyhow::Result<f64> {
        let v: f64 = s.trim_end_matches('%').parse()?;
        anyhow::ensure!(
            v.is_finite() && (0.0..=100.0).contains(&v),
            "use a value from 0 to 100"
        );
        Ok(v)
    };
    let action = match words {
        [] | ["status"] => {
            if let Some(status) = player::status() {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("player stopped");
            }
            return Ok(());
        }
        ["cart" | "carts", rest @ ..] => return cart(rest),
        ["skip", v] => {
            let seconds: f64 = v.trim_end_matches('s').parse()?;
            anyhow::ensure!(seconds.is_finite(), "use seconds, such as -10 or 30");
            Action::Skip(seconds)
        }
        ["load", path] => Action::Load(path.to_string()),
        ["add", files @ ..] if !files.is_empty() => {
            // Paths as the shell gave them: the player runs elsewhere.
            let files = files
                .iter()
                .map(|f| match std::fs::canonicalize(f) {
                    Ok(path) if !f.contains("://") => path.to_string_lossy().into_owned(),
                    _ => f.to_string(),
                })
                .collect();
            Action::Enqueue(files)
        }
        ["queue"] => {
            let status = player::status();
            let (queue, at) = status.map(|s| (s.queue, s.queue_at)).unwrap_or_default();
            for (i, path) in queue.iter().enumerate() {
                println!("{} {:>3}  {path}", if i as i64 == at { ">" } else { " " }, i + 1);
            }
            return Ok(());
        }
        ["queue", "clear"] => Action::ClearQueue,
        ["queue", "shuffle"] => Action::Shuffle,
        ["queue", n] => {
            let n: usize = n.parse()?;
            anyhow::ensure!(n >= 1, "the queue counts from 1");
            Action::Jump(n - 1)
        }
        ["next"] => Action::Next,
        ["prev" | "previous"] => Action::Previous,
        ["speed", v] => {
            let speed: f64 = v.trim_end_matches('x').parse()?;
            anyhow::ensure!((0.5..=2.0).contains(&speed), "use a speed from 0.5 to 2");
            Action::Speed(speed)
        }
        ["repeat", "off"] => Action::Repeat(0),
        ["repeat", "one"] => Action::Repeat(1),
        ["repeat", "all"] => Action::Repeat(2),
        ["ab"] => Action::AbLoop,
        ["duck", "on"] => Action::Duck(true, None),
        ["duck", "off"] => Action::Duck(false, None),
        // A cart's voice says it ended: it no longer counts.
        ["duck", "off", voice] => Action::Duck(false, voice.parse().ok()),
        ["goto", v] => {
            let seconds: f64 = v.trim_end_matches('s').parse()?;
            anyhow::ensure!(seconds.is_finite() && seconds >= 0.0, "use seconds from the start");
            Action::Goto(seconds)
        }
        ["mark", name @ ..] => {
            let status = player::snapshot().filter(|s| s.ready && !s.path.is_empty());
            let status = status.ok_or_else(|| anyhow::anyhow!("open a file before marking a place"))?;
            let mut marks = player::marks::Marks::load();
            let index = marks.add(&status.path, status.position);
            if !name.is_empty() {
                marks.rename(&status.path, index, &name.join(" "));
            }
            marks.save();
            println!("{}", player::marks::stamp_of(status.position));
            return Ok(());
        }
        ["marks"] => {
            // The loaded file's chapters and marks, as Copy gives them.
            let status = player::snapshot().ok_or_else(|| anyhow::anyhow!("player stopped"))?;
            let marks = player::marks::Marks::load();
            let places: Vec<(f64, String)> = status
                .chapters
                .iter()
                .enumerate()
                .map(|(i, at)| (*at, status.chapter_titles.get(i).cloned().unwrap_or_default()))
                .chain(marks.of(&status.path).iter().map(|m| (m.at, m.name.clone())))
                .collect();
            println!("{}", player::marks::notes(&places));
            return Ok(());
        }
        ["chapter", "next"] => Action::Chapter(1),
        ["chapter", "prev" | "previous"] => Action::Chapter(-1),
        ["play"] => Action::Play(true),
        ["pause"] => Action::Play(false),
        ["toggle"] => Action::Toggle,
        ["stop"] => Action::Stop,
        ["quit"] => Action::Quit,
        ["seek", v] => Action::Seek(number(v)?),
        ["volume", v] => Action::Volume(number(v)?),
        ["loop", "on"] => Action::Loop(true),
        ["loop", "off"] => Action::Loop(false),
        ["cue"] => Action::Cue,
        ["pfl"] | ["pfl", "on"] => Action::Pfl(true),
        ["pfl", "off"] => Action::Pfl(false),
        _ => anyhow::bail!(
            "player load <file|URL> | play | pause | toggle | stop | seek <0..100%> | skip <seconds> | volume <0..100> | speed <0.5..2> | loop on|off | repeat off|one|all | ab | cue | pfl [on|off] | status | quit\nplayer add <file|URL>… | next | prev | queue [<n>|clear|shuffle] | chapter next|prev | goto <seconds> | mark [name] | marks\n{CART_USAGE}"
        ),
    };
    player::command(action).map_err(anyhow::Error::msg)
}
