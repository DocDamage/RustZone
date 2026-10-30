use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use crate::dsp::{EqChannelMode, EqFilterType, MasteringSuite, MatchEq, ModuleKind};

/// Version string written into presets saved from the built-in DSP suite
pub const DSP_PRESET_VERSION: &str = "MasteringStudio-DSP-2";

const CHAIN_ELEMENT: &str = "chain";
const MATCH_ELEMENT: &str = "match_eq_learned";
const ENABLED_PARAM: &str = "Enabled";
const AUTO_GAIN_PARAM: &str = "Auto gain";

/// An XML preset: the app's own module presets, or factory presets in the element/param layout
#[derive(Clone, Debug, Default)]
pub struct XmlPreset {
    pub name: String,
    pub plugin_ver: String,
    pub comments: String,
    pub params: HashMap<String, f32>,
}

fn id_to_shape(id: f32) -> EqFilterType {
    match id as i32 {
        0 => EqFilterType::LowShelf,
        1 => EqFilterType::Peaking,
        2 => EqFilterType::HighShelf,
        3 => EqFilterType::HighPass,
        4 => EqFilterType::LowPass,
        _ => EqFilterType::Peaking,
    }
}

fn id_to_channel_mode(id: f32) -> EqChannelMode {
    match id as i32 {
        1 => EqChannelMode::MidOnly,
        2 => EqChannelMode::SideOnly,
        _ => EqChannelMode::Stereo,
    }
}

fn flag(v: bool) -> f32 {
    if v { 1.0 } else { 0.0 }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

impl XmlPreset {
    /// Parse an XML preset file (.xml) without external XML dependencies
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| format!("Failed to read XML preset file: {}", e))?;
        let name = path
            .as_ref()
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        Self::parse_str(&content, name)
    }

    /// Parse XML string content
    pub fn parse_str(xml: &str, preset_name: String) -> Result<Self, String> {
        let mut preset = XmlPreset {
            name: preset_name,
            plugin_ver: String::new(),
            comments: String::new(),
            params: HashMap::new(),
        };

        // Root tag attributes e.g. Comments="..." PluginVersion="..."
        if let Some(c) = extract_attribute(xml, "Comments") {
            preset.comments = xml_unescape(c);
        }
        if let Some(v) = extract_attribute(xml, "PluginVersion") {
            preset.plugin_ver = xml_unescape(v);
        }

        // Parse <Param ElementID="..." ParamID="..." Value="..." />
        let mut cursor = 0;
        while let Some(start_pos) = xml[cursor..].find("<Param ") {
            let tag_start = cursor + start_pos;
            let tag_content = match xml[tag_start..].find("/>") {
                Some(end_pos) => &xml[tag_start..tag_start + end_pos],
                None => {
                    cursor = tag_start + 7;
                    continue;
                }
            };

            let element_id = extract_attribute(tag_content, "ElementID");
            let param_id = extract_attribute(tag_content, "ParamID");
            let value_str = extract_attribute(tag_content, "Value");

            if let (Some(elem), Some(param), Some(val_str)) = (element_id, param_id, value_str) {
                if let Ok(val) = val_str.parse::<f32>() {
                    let key = format!("{}::{}", xml_unescape(elem), xml_unescape(param));
                    preset.params.insert(key, val);
                }
            }

            cursor = tag_start + tag_content.len() + 2;
        }

        Ok(preset)
    }

    /// Capture the whole suite: chain order, every module's on/off state and parameters, and any
    /// learned Match EQ spectra. `apply_to_suite` reads it back.
    pub fn from_suite(suite: &MasteringSuite, name: &str) -> Self {
        let mut preset = XmlPreset {
            name: name.to_string(),
            plugin_ver: DSP_PRESET_VERSION.to_string(),
            comments: "Saved from the built-in mastering suite".to_string(),
            params: HashMap::new(),
        };

        for (slot, kind) in suite.chain.iter().enumerate() {
            let id = ModuleKind::ALL.iter().position(|k| k == kind).unwrap_or(0);
            preset.params.insert(format!("{}::Slot {}", CHAIN_ELEMENT, slot), id as f32);
        }

        for &kind in ModuleKind::ALL {
            let module = suite.module(kind);
            preset.params.insert(format!("{}::{}", kind.key(), ENABLED_PARAM), flag(module.enabled()));
            preset.params.insert(format!("{}::{}", kind.key(), AUTO_GAIN_PARAM), flag(suite.auto_gain[kind.index()]));
            for (i, info) in module.params().iter().enumerate() {
                preset.params.insert(format!("{}::{}", kind.key(), info.name), module.get_param(i));
            }
        }

        let (src, refr) = suite.match_eq.learned_spectra();
        for (i, v) in src.iter().enumerate() {
            preset.params.insert(format!("{}::Source {}", MATCH_ELEMENT, i), *v);
        }
        for (i, v) in refr.iter().enumerate() {
            preset.params.insert(format!("{}::Reference {}", MATCH_ELEMENT, i), *v);
        }

        preset
    }

    /// Serialize as an XML preset (sorted for stable, diffable output)
    pub fn to_xml_string(&self) -> String {
        let mut keys: Vec<&String> = self.params.keys().collect();
        keys.sort();
        let mut xml = String::new();
        let _ = writeln!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        let _ = writeln!(
            xml,
            "<Preset Name=\"{}\" PluginVersion=\"{}\" Comments=\"{}\">",
            xml_escape(&self.name),
            xml_escape(&self.plugin_ver),
            xml_escape(&self.comments)
        );
        for key in keys {
            if let Some((elem, param)) = key.split_once("::") {
                let _ = writeln!(
                    xml,
                    "  <Param ElementID=\"{}\" ParamID=\"{}\" Value=\"{}\" />",
                    xml_escape(elem),
                    xml_escape(param),
                    self.params[key]
                );
            }
        }
        xml.push_str("</Preset>\n");
        xml
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        std::fs::write(path, self.to_xml_string()).map_err(|e| format!("Failed to write XML preset: {}", e))
    }

    /// Apply the preset to the suite. Reads both this app's own module layout and the older
    /// element/param layout (ElementID "Maximizer", "Equalizer", ...) used by factory XML presets.
    pub fn apply_to_suite(&self, suite: &mut MasteringSuite) {
        self.apply_legacy(suite);
        self.apply_modules(suite);
    }

    fn apply_modules(&self, suite: &mut MasteringSuite) {
        // Chain order, only if complete
        let order: Vec<ModuleKind> = (0..ModuleKind::ALL.len())
            .map_while(|slot| self.get_param(CHAIN_ELEMENT, &format!("Slot {}", slot)))
            .filter_map(|&id| ModuleKind::ALL.get(id as usize).copied())
            .collect();
        if !order.is_empty() {
            suite.set_chain(order);
        }

        for &kind in ModuleKind::ALL {
            if let Some(&ag) = self.get_param(kind.key(), AUTO_GAIN_PARAM) {
                suite.set_auto_gain(kind, ag > 0.5);
            }
            let infos = suite.module(kind).params();
            let module = suite.module_mut(kind);
            if let Some(&en) = self.get_param(kind.key(), ENABLED_PARAM) {
                module.set_enabled(en > 0.5);
            }
            for (i, info) in infos.iter().enumerate() {
                if let Some(&v) = self.get_param(kind.key(), &info.name) {
                    module.set_param(i, v);
                }
            }
        }

        let n = MatchEq::learned_point_count();
        let read = |prefix: &str| -> Option<Vec<f32>> {
            (0..n).map(|i| self.get_param(MATCH_ELEMENT, &format!("{} {}", prefix, i)).copied()).collect()
        };
        if let (Some(src), Some(refr)) = (read("Source"), read("Reference")) {
            suite.match_eq.set_learned_spectra(src, refr);
        }
    }

    /// Element/param layout of factory XML presets
    fn apply_legacy(&self, suite: &mut MasteringSuite) {
        let enabled = |elem: &str, param: &str| self.get_param(elem, param).map(|&v| v > 0.5);

        // 1. Maximizer Parameters
        if let Some(en) = enabled("Maximizer", "Enable") {
            suite.maximizer.enabled = en;
        }
        if let Some(&thresh) = self.get_param("Maximizer", "Threshold") {
            suite.maximizer.threshold_db = thresh.clamp(-30.0, 0.0);
        }
        if let Some(&margin) = self.get_param("Maximizer", "Margin") {
            suite.maximizer.ceiling_db = margin.clamp(-12.0, 0.0);
        }
        if let Some(&char_time) = self.get_param("Maximizer", "Character") {
            // Character ranges from ~0.5 to 10.0 (maps to adaptive release 20ms to 250ms)
            suite.maximizer.release_ms = (char_time * 30.0).clamp(10.0, 300.0);
        }
        if let Some(irc) = enabled("Maximizer", "IRC Adaptive") {
            suite.maximizer.irc_adaptive = irc;
        }

        // 2. Equalizer Parameters (8 Bands)
        if let Some(en) = enabled("Equalizer", "Enable") {
            suite.dynamic_eq.enabled = en;
        }
        for band in 1..=suite.dynamic_eq.bands.len() {
            let p = |name: &str| self.get_param("Equalizer", &format!("Band {} {}", band, name)).copied();
            let b = &mut suite.dynamic_eq.bands[band - 1];

            if let Some(en) = p("Enable") {
                b.enabled = en > 0.5;
            }
            if let Some(g) = p("Gain") {
                b.static_gain_db = g.clamp(-18.0, 18.0);
            }
            if let Some(f) = p("Frequency") {
                b.freq = f.clamp(20.0, 20000.0);
            }
            if let Some(qv) = p("Q") {
                b.q = qv.clamp(0.2, 10.0);
            }
            if let Some(s) = p("Shape") {
                b.filter_type = id_to_shape(s);
            }
            if let Some(m) = p("Channel Mode") {
                b.channel_mode = id_to_channel_mode(m);
            }
            if let Some(d) = p("Dynamic Enable") {
                b.dynamic_enabled = d > 0.5;
            }
            if let Some(t) = p("Dynamic Threshold") {
                b.threshold_db = t.clamp(-60.0, 0.0);
            }
            if let Some(r) = p("Dynamic Range") {
                b.dynamic_range_db = r.clamp(-12.0, 12.0);
            }
            b.update_coefficients(b.static_gain_db);
        }

        // 3. Multiband Dynamics
        if let Some(en) = enabled("Dynamics", "Enable") {
            suite.multiband_dynamics.enabled = en;
        }
        for band in 1..=suite.multiband_dynamics.bands.len() {
            let p = |name: &str| self.get_param("Dynamics", &format!("Band {} {}", band, name)).copied();
            let b = &mut suite.multiband_dynamics.bands[band - 1];
            if let Some(en) = p("Enable") {
                b.enabled = en > 0.5;
            }
            if let Some(v) = p("Threshold") {
                b.threshold_db = v.clamp(-60.0, 0.0);
            }
            if let Some(v) = p("Ratio") {
                b.ratio = v.clamp(1.0, 20.0);
            }
            if let Some(v) = p("Attack") {
                b.attack_ms = v.clamp(0.1, 500.0);
            }
            if let Some(v) = p("Release") {
                b.release_ms = v.clamp(1.0, 2000.0);
            }
            if let Some(v) = p("Makeup") {
                b.makeup_gain_db = v.clamp(-12.0, 12.0);
            }
        }

        // 4. Imager Parameters
        if let Some(en) = enabled("Imager", "Enable") {
            suite.imager.enabled = en;
        }
        if let Some(&w) = self.get_param("Imager", "Width") {
            suite.imager.width = (w / 100.0).clamp(0.0, 2.0);
        }
        if let Some(&mono_freq) = self.get_param("Imager", "Mono Frequency") {
            suite.imager.set_mono_cutoff(mono_freq);
        }

        // 5. Exciter Parameters
        if let Some(en) = enabled("Exciter", "Enable") {
            suite.exciter.enabled = en;
        }
        if let Some(&drv) = self.get_param("Exciter", "Drive") {
            suite.exciter.drive = (drv / 100.0).clamp(0.0, 1.0);
        }
        if let Some(&mix) = self.get_param("Exciter", "Mix") {
            suite.exciter.mix = (mix / 100.0).clamp(0.0, 1.0);
        }
        if let Some(&warmth) = self.get_param("Exciter", "Warmth") {
            suite.exciter.warmth = (warmth / 100.0).clamp(0.0, 1.0);
        }

        // 6. Clarity Parameters
        if let Some(en) = enabled("Clarity", "Enable") {
            suite.clarity.enabled = en;
        }
        if let Some(&amount) = self.get_param("Clarity", "Amount") {
            suite.clarity.amount = (amount / 100.0).clamp(0.0, 1.0);
        }
        if let Some(&focus) = self.get_param("Clarity", "Focus Frequency") {
            suite.clarity.set_focus_freq(focus);
        }
    }

    fn get_param(&self, element: &str, param: &str) -> Option<&f32> {
        let key = format!("{}::{}", element, param);
        self.params.get(&key)
    }
}

/// Helper to parse XML attribute values e.g. key="value"
fn extract_attribute<'a>(tag: &'a str, attr_name: &str) -> Option<&'a str> {
    let pattern = format!("{}=\"", attr_name);
    let mut search_from = 0;
    while let Some(pos) = tag[search_from..].find(&pattern) {
        let abs = search_from + pos;
        // Require a whitespace boundary so "ID" doesn't match inside "ElementID"
        if abs == 0 || tag[..abs].ends_with(char::is_whitespace) {
            let rest = &tag[abs + pattern.len()..];
            let end = rest.find('"')?;
            return Some(&rest[..end]);
        }
        search_from = abs + pattern.len();
    }
    None
}
