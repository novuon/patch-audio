//! Explicit physical-port assignments, outside the real-time callback.
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Routing {
    pub main: BTreeMap<String, Vec<String>>,
    pub cue: Option<Assignment>,
    /// The Mixer's AUX 1 and 2, to physical ports (none: their virtual
    /// sources only).
    pub aux1: Option<Assignment>,
    pub aux2: Option<Assignment>,
}

impl Routing {
    pub fn aux(&self, bus: &str) -> Option<&Assignment> {
        match bus {
            "aux1" => self.aux1.as_ref(),
            "aux2" => self.aux2.as_ref(),
            _ => None,
        }
    }

    pub fn aux_mut(&mut self, bus: &str) -> Option<&mut Option<Assignment>> {
        match bus {
            "aux1" => Some(&mut self.aux1),
            "aux2" => Some(&mut self.aux2),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Assignment {
    pub device: String,
    pub ports: Vec<String>,
}

fn path() -> std::path::PathBuf {
    crate::session::state_dir().join("output-routing.json")
}
impl Routing {
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        let file = path();
        std::fs::create_dir_all(file.parent().ok_or("no routing directory")?)
            .map_err(|e| e.to_string())?;
        let temporary = file.with_extension("json.new");
        std::fs::write(
            &temporary,
            serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(temporary, file).map_err(|e| e.to_string())
    }
}
pub fn validate(ports: &[String], available: &[String]) -> Result<(), String> {
    if !(1..=2).contains(&ports.len()) || (ports.len() == 2 && ports[0] == ports[1]) {
        return Err("choose one mono port or two distinct stereo ports".into());
    }
    if ports.iter().any(|p| !available.contains(p)) {
        return Err("the selected output port is not available on this device".into());
    }
    Ok(())
}
pub fn links(ports: &[String], bus: &str) -> Vec<(String, String)> {
    match ports {
        [mono] if bus == "out" => {
            vec![(format!("{}:out_MONO", crate::links::ENGINE), mono.clone())]
        }
        [mono] => vec![
            (format!("{}:{bus}_FL", crate::links::ENGINE), mono.clone()),
            (format!("{}:{bus}_FR", crate::links::ENGINE), mono.clone()),
        ],
        [left, right] => vec![
            (format!("{}:{bus}_FL", crate::links::ENGINE), left.clone()),
            (format!("{}:{bus}_FR", crate::links::ENGINE), right.clone()),
        ],
        _ => vec![],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_names_and_non_first_pairs_are_exact() {
        let ports = vec![
            "interface:playback_AUX6".into(),
            "interface:playback_AUX7".into(),
        ];
        assert!(validate(&ports, &ports).is_ok());
        let actual = links(&ports, "out");
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[0].1, ports[0]);
        assert_eq!(actual[1].1, ports[1]);
        assert!(validate(&[ports[0].clone(), ports[0].clone()], &ports).is_err());
        assert!(validate(&ports, &[]).is_err());
    }
}
