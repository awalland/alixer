//! The deliberately small set of effects offered by Alixer's insert picker.
//! Unknown saved inserts remain playable and editable for backwards compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Equalizer,
    Compressor,
    Limiter,
}
impl Effect {
    pub const ALL: [Self; 3] = [Self::Equalizer, Self::Compressor, Self::Limiter];
    pub fn name(self) -> &'static str {
        match self {
            Self::Equalizer => "Parametric EQ",
            Self::Compressor => "Compressor",
            Self::Limiter => "Limiter",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Equalizer => "Eight parametric bands · filter types, slope, Q and gain",
            Self::Compressor => "Dynamics · attack, release, knee and sidechain filtering",
            Self::Limiter => "Peak control · lookahead, oversampling and stereo linking",
        }
    }
    pub fn uri(self, channels: usize) -> Option<String> {
        let layout = match channels {
            1 => "mono",
            2 => "stereo",
            _ => return None,
        };
        let plugin = match self {
            Self::Equalizer => "para_equalizer_x8",
            Self::Compressor => "compressor",
            Self::Limiter => "limiter",
        };
        Some(format!("http://lsp-plug.in/plugins/lv2/{plugin}_{layout}"))
    }
    pub fn from_uri(uri: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| {
            [1, 2]
                .into_iter()
                .any(|n| effect.uri(n).as_deref() == Some(uri))
        })
    }
}
