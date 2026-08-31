/// Vendor identity resolution: raw strings from bundle metadata are messy
/// ("Plugin-alliance", "Uaudio", "iZotope, Inc."), so everything funnels
/// through an alias map before it reaches the catalog.

const ALIASES: &[(&[&str], &str)] = &[
    (&["plugin-alliance", "plugin alliance gmbh", "brainworx"], "Plugin Alliance"),
    (&["izotope", "izotope, inc.", "izotope inc."], "iZotope"),
    (&["uaudio", "universal audio (uadx)", "universal audio, inc."], "Universal Audio"),
    (&["pspaudioware", "pspaudioware.com", "psp audioware"], "PSP Audioware"),
    (&["ikmultimedia", "ik multimedia us, llc"], "IK Multimedia"),
    (&["native-instruments", "native instruments gmbh"], "Native Instruments"),
    (&["_510k"], "510k"),
    (&["_futurephonic"], "Futurephonic"),
    (&["fabfilter"], "FabFilter"),
    (&["soundtoys", "sound toys"], "Soundtoys"),
    (&["valhalladsp", "valhalla dsp, llc", "valhalla dsp"], "Valhalla DSP"),
    (&["air music technology", "air music tech", "airmusictech"], "AIR Music Technology"),
    (&["ssl", "solid state logic"], "Solid State Logic"),
    (&["waves", "waves audio ltd."], "Waves"),
    (&["korg", "korg inc."], "KORG"),
    (&["roland", "roland cloud", "rolandcloud"], "Roland"),
    (&["antares", "antares audio technologies"], "Antares"),
    (&["xlnaudio", "xln audio ab"], "XLN Audio"),
    (&["d16-group", "d16 group audio software"], "D16 Group"),
    (&["sugar-bytes", "sugar bytes gmbh"], "Sugar Bytes"),
    (&["meldaproduction", "melda"], "MeldaProduction"),
    (&["tdr", "tokyo dawn records", "tokyo dawn labs"], "Tokyo Dawn Labs"),
    (&["goodhertz", "goodhertz inc", "goodhertz, inc."], "Goodhertz"),
    (&["neuraldsp", "neural dsp technologies"], "Neural DSP"),
    (&["cherryaudio", "cherry audio llc"], "Cherry Audio"),
    (&["gforce", "gforce software"], "GForce Software"),
    (&["eastwest", "east west sounds"], "EastWest"),
    (&["u-he", "urs heckmann"], "u-he"),
    (&["toontrack", "toontrack music ab"], "Toontrack"),
    (&["eventide", "eventide inc"], "Eventide"),
    (&["softube", "softube ab"], "Softube"),
    (&["arturia", "arturia sa"], "Arturia"),
    (&["apple", "apple, inc.", "apple inc."], "Apple"),
    (&["steinberg", "steinberg media technologies gmbh"], "Steinberg"),
    (&["celemony", "celemony software gmbh"], "Celemony"),
    (&["sonnox", "sonnox ltd"], "Sonnox"),
    (&["kilohearts", "kilohearts ab"], "Kilohearts"),
    (&["bluecataudio", "blue cat audio"], "Blue Cat Audio"),
    (&["safaripedals", "safari pedals"], "Safari Pedals"),
    (&["sonible", "sonible gmbh"], "sonible"),
    (&["noiseash", "noiseash audio"], "NoiseAsh"),
    (&["mlsoundlab", "ml sound lab"], "ML Sound Lab"),
    (&["ujam"], "UJAM"),
    (&["bogrendigital", "bogren digital"], "Bogren Digital"),
    (&["auroradsp", "aurora dsp"], "Aurora DSP"),
    (&["babyaudio", "baby audio"], "BABY Audio"),
    (&["kiiveaudio", "kiive audio", "kiive", "mycompany"], "Kiive Audio"),
    // Mappings below were verified by the resolver-sweep research agents
    // (2026-08-30) from product-name evidence; several are JUCE-template
    // junk ("MyCompany") or per-format metadata variants.
    (&["wizoo"], "AIR Music Technology"),
    (&["audiodamage"], "Audio Damage, Inc."),
    (&["moogmusic", "moog music"], "Moog"),
    (&["lunacy"], "Lunacy Audio"),
    (&["datamind"], "DataMind Audio"),
    (&["xfer"], "Xfer Records"),
    (&["uvisoundsource"], "UVI"),
    (&["wavesequencer"], "wavesequencer.com"),
    (&["plogue art et technologie"], "Plogue"),
    (&["synth", "fx", "fx16x16"], "Native Instruments"),
    (&["audiounit"], "Synapse Audio"),
    (&["uk"], "Mastering the Mix"),
    (&["distfilter"], "Diginoiz"),
    (&["vplus", "vplusinst", "polykb2", "polykb3", "xils201", "xils5000", "cs80"], "XILS-lab"),
    (&["process"], "Process.audio"),
    (&["aurora"], "Aurora DSP"),
    (&["decided"], "Decidedly"),
    (&["dynassist"], "NoiseWorks"),
    (&["musikhackmp15"], "Musik Hack"),
    (&["akai", "akaipro"], "Akai Professional"),
    (&["adptraudio"], "ADPTR"),
    (&["vital"], "Vital Audio"),
    (&["twonotes"], "Two notes Audio Engineering"),
    (&["deskew"], "Deskew Technologies, LLC"),
    (&["phaseburn"], "Phaseburn Music"),
    (&["schulz"], "schulz.audio"),
    (&["safariaudio", "super keys", "superkeys"], "Safari Pedals"),
    (&["higherplane", "higher-plane"], "Higher Plane Software"),
    (&["synthfactory"], "TheSynthFactory"),
    (&["thxltd"], "THX Ltd"),
    (&["atkinsonadvancedmodeling"], "Atkinson Advanced Modeling, LLC"),
    (&["aguilaramp"], "Aguilar"),
    (&["fineclassics"], "Fine Classics Plugins"),
    (&["tbtech"], "Threebodytech"),
    (&["applied-acoustics"], "Applied Acoustics Systems"),
    (&["kx"], "devi ever fx"),
    (&["yourcompany"], "Evabeat"),
    (&["filterverse", "polyverse"], "Polyverse Music"),
    (&["lindellaudio", "lindell audio"], "Lindellplugins"),
];

/// Vendors whose updates are owned by their own manager app; plugscan
/// deep-links to these instead of resolving versions itself.
const MANAGERS: &[(&str, &str)] = &[
    ("Native Instruments", "Native Access"),
    ("iZotope", "Native Access"),
    ("Universal Audio", "UA Connect"),
    ("Plugin Alliance", "PA Installation Manager"),
    ("Waves", "Waves Central"),
    ("Arturia", "Arturia Software Center"),
    ("IK Multimedia", "IK Product Manager"),
    ("Toontrack", "Toontrack Product Manager"),
    ("Roland", "Roland Cloud Manager"),
    ("KORG", "KORG Software Pass"),
    ("Steinberg", "Steinberg Download Assistant"),
    ("XLN Audio", "XLN Online Installer"),
    ("Antares", "Auto-Tune Central"),
    ("Softube", "Softube Central"),
    ("Slate Digital", "Slate All Access"),
    ("EastWest", "EastWest Installation Center"),
];

/// Identity key: lowercase alphanumerics only, so "D16 Group", "d16-group"
/// and "D16group" all collide.
pub fn normkey(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn canonical(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }
    let key = normkey(trimmed);
    for (aliases, canon) in ALIASES {
        if normkey(canon) == key || aliases.iter().any(|a| normkey(a) == key) {
            return (*canon).to_string();
        }
    }
    trimmed.to_string()
}

pub fn manager_app(canonical_name: &str) -> Option<&'static str> {
    MANAGERS
        .iter()
        .find(|(v, _)| *v == canonical_name)
        .map(|(_, m)| *m)
}

/// Bundles with unusable vendor metadata, mapped by product name when
/// same-name inference has nothing to latch onto.
const PRODUCT_VENDOR: &[(&str, &str)] = &[
    ("SpectraLayers", "Steinberg"),
    // XILS VST3 bundles are named differently from their AU twins
    // ("polyKB II" vs "polyKB II_x64"), so same-name inference can't match.
    ("polyKB II", "XILS-lab"),
    ("PolyKB III", "XILS-lab"),
    ("TheEighty", "XILS-lab"),
    ("XILS 201", "XILS-lab"),
    ("XILS 5000", "XILS-lab"),
];

pub fn vendor_for_product(product: &str) -> Option<&'static str> {
    PRODUCT_VENDOR
        .iter()
        .find(|(p, _)| p.eq_ignore_ascii_case(product))
        .map(|(_, v)| *v)
}

/// Vendor strings that carry no identity (broken bundle metadata).
pub fn is_junk_vendor(name: &str) -> bool {
    matches!(normkey(name).as_str(), "vst3" | "" | "?")
}
