use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeGroup {
  Dark,
  Light,
  Powder,
}

impl ThemeGroup {
  pub fn label(self) -> &'static str {
    match self {
      ThemeGroup::Dark => "Dark",
      ThemeGroup::Light => "Light",
      ThemeGroup::Powder => "Powder",
    }
  }
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeInfo {
  pub id: &'static str,
  pub name: &'static str,
  pub group: ThemeGroup,
  pub is_dark: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ThemeId {
  #[default]
  Dark,
  Amoled,
  SolarizedDark,
  GruvboxDark,
  Nord,
  TokyoNight,
  CatppuccinMocha,
  Light,
  SolarizedLight,
  PowderBlue,
  PowderSage,
  PowderLavender,
  PowderRose,
  PowderMint,
  PowderSand,
  PowderSlate,
}

const ALL_THEMES: &[ThemeId] = &[
  ThemeId::Dark,
  ThemeId::Amoled,
  ThemeId::SolarizedDark,
  ThemeId::GruvboxDark,
  ThemeId::Nord,
  ThemeId::TokyoNight,
  ThemeId::CatppuccinMocha,
  ThemeId::Light,
  ThemeId::SolarizedLight,
  ThemeId::PowderBlue,
  ThemeId::PowderSage,
  ThemeId::PowderLavender,
  ThemeId::PowderRose,
  ThemeId::PowderMint,
  ThemeId::PowderSand,
  ThemeId::PowderSlate,
];

impl ThemeId {
  pub fn all() -> &'static [ThemeId] {
    ALL_THEMES
  }

  pub fn label(self) -> &'static str {
    self.info().id
  }

  pub fn cycle(self) -> ThemeId {
    let all = Self::all();
    let idx = all.iter().position(|id| *id == self).unwrap_or(0);
    all[(idx + 1) % all.len()]
  }

  pub fn from_id(id: &str) -> Option<ThemeId> {
    match id {
      "dark" | "Dark" => Some(ThemeId::Dark),
      "light" | "Light" => Some(ThemeId::Light),
      "amoled" | "Amoled" => Some(ThemeId::Amoled),
      "solarized-dark" | "SolarizedDark" => Some(ThemeId::SolarizedDark),
      "solarized-light" | "SolarizedLight" => Some(ThemeId::SolarizedLight),
      "gruvbox-dark" | "GruvboxDark" => Some(ThemeId::GruvboxDark),
      "nord" | "Nord" => Some(ThemeId::Nord),
      "tokyo-night" | "TokyoNight" => Some(ThemeId::TokyoNight),
      "catppuccin-mocha" | "CatppuccinMocha" => Some(ThemeId::CatppuccinMocha),
      "powder-blue" | "PowderBlue" => Some(ThemeId::PowderBlue),
      "powder-sage" | "PowderSage" => Some(ThemeId::PowderSage),
      "powder-lavender" | "PowderLavender" => Some(ThemeId::PowderLavender),
      "powder-rose" | "PowderRose" => Some(ThemeId::PowderRose),
      "powder-mint" | "PowderMint" => Some(ThemeId::PowderMint),
      "powder-sand" | "PowderSand" => Some(ThemeId::PowderSand),
      "powder-slate" | "PowderSlate" => Some(ThemeId::PowderSlate),
      _ => None,
    }
  }

  pub fn info(self) -> ThemeInfo {
    match self {
      ThemeId::Dark => ThemeInfo {
        id: "dark",
        name: "Dark",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::Amoled => ThemeInfo {
        id: "amoled",
        name: "AMOLED",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::SolarizedDark => ThemeInfo {
        id: "solarized-dark",
        name: "Solarized Dark",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::GruvboxDark => ThemeInfo {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::Nord => ThemeInfo {
        id: "nord",
        name: "Nord",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::TokyoNight => ThemeInfo {
        id: "tokyo-night",
        name: "Tokyo Night",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::CatppuccinMocha => ThemeInfo {
        id: "catppuccin-mocha",
        name: "Catppuccin Mocha",
        group: ThemeGroup::Dark,
        is_dark: true,
      },
      ThemeId::Light => ThemeInfo {
        id: "light",
        name: "Light",
        group: ThemeGroup::Light,
        is_dark: false,
      },
      ThemeId::SolarizedLight => ThemeInfo {
        id: "solarized-light",
        name: "Solarized Light",
        group: ThemeGroup::Light,
        is_dark: false,
      },
      ThemeId::PowderBlue => ThemeInfo {
        id: "powder-blue",
        name: "Powder Blue",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderSage => ThemeInfo {
        id: "powder-sage",
        name: "Powder Sage",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderLavender => ThemeInfo {
        id: "powder-lavender",
        name: "Powder Lavender",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderRose => ThemeInfo {
        id: "powder-rose",
        name: "Powder Rose",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderMint => ThemeInfo {
        id: "powder-mint",
        name: "Powder Mint",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderSand => ThemeInfo {
        id: "powder-sand",
        name: "Powder Sand",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
      ThemeId::PowderSlate => ThemeInfo {
        id: "powder-slate",
        name: "Powder Slate",
        group: ThemeGroup::Powder,
        is_dark: false,
      },
    }
  }

  pub fn theme(self) -> Theme {
    match self {
      ThemeId::Dark => Theme::dark(),
      ThemeId::Amoled => Theme::amoled(),
      ThemeId::SolarizedDark => Theme::solarized_dark(),
      ThemeId::GruvboxDark => Theme::gruvbox_dark(),
      ThemeId::Nord => Theme::nord(),
      ThemeId::TokyoNight => Theme::tokyo_night(),
      ThemeId::CatppuccinMocha => Theme::catppuccin_mocha(),
      ThemeId::Light => Theme::light(),
      ThemeId::SolarizedLight => Theme::solarized_light(),
      ThemeId::PowderBlue => Theme::powder_blue(),
      ThemeId::PowderSage => Theme::powder_sage(),
      ThemeId::PowderLavender => Theme::powder_lavender(),
      ThemeId::PowderRose => Theme::powder_rose(),
      ThemeId::PowderMint => Theme::powder_mint(),
      ThemeId::PowderSand => Theme::powder_sand(),
      ThemeId::PowderSlate => Theme::powder_slate(),
    }
  }
}

impl Serialize for ThemeId {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    serializer.serialize_str(self.info().id)
  }
}

impl<'de> Deserialize<'de> for ThemeId {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let s = String::deserialize(deserializer)?;
    Self::from_id(&s)
      .ok_or_else(|| serde::de::Error::custom(format!("unknown theme: {s}")))
  }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
  pub accent: Color,
  pub header: Color,
  pub text: Color,
  pub text_dim: Color,
  pub border: Color,
  pub border_active: Color,
  pub bg: Color,
  pub bg_panel: Color,
  pub bg_input: Color,
  pub bg_selection: Color,
  pub bg_code: Color,
  pub bg_chat: Color,
  pub bg_user_msg: Color,
  pub bg_popup: Color,
  pub text_on_accent: Color,
  pub success: Color,
  pub warning: Color,
  pub error: Color,
  pub math: Color,
  pub mono: Color,
  pub rule: Color,
  pub toc_dim: Color,
  pub bookmark_bg: Color,
  pub cursor_bg: Color,
  pub cursor_fg: Color,
  pub search_match_bg: Color,
  pub search_match_fg: Color,
}

impl Theme {
  pub fn dark() -> Self {
    Self {
      accent: Color::Rgb(135, 206, 235),
      header: Color::Rgb(62, 126, 180),
      text: Color::Rgb(218, 224, 230),
      text_dim: Color::Rgb(105, 118, 130),
      border: Color::Rgb(48, 60, 72),
      border_active: Color::Rgb(80, 96, 112),
      bg: Color::Black,
      bg_panel: Color::Black,
      bg_input: Color::Rgb(22, 31, 40),
      bg_selection: Color::Rgb(42, 58, 72),
      bg_code: Color::Rgb(28, 28, 28),
      bg_chat: Color::Rgb(18, 18, 18),
      bg_user_msg: Color::Rgb(35, 35, 35),
      bg_popup: Color::Black,
      text_on_accent: Color::Black,
      success: Color::Rgb(132, 190, 145),
      warning: Color::Rgb(204, 180, 105),
      error: Color::Red,
      math: Color::Rgb(80, 200, 160),
      mono: Color::Rgb(180, 160, 120),
      rule: Color::Rgb(55, 65, 80),
      toc_dim: Color::Rgb(80, 95, 115),
      bookmark_bg: Color::Rgb(55, 45, 15),
      cursor_bg: Color::White,
      cursor_fg: Color::Black,
      search_match_bg: Color::Yellow,
      search_match_fg: Color::Black,
    }
  }

  pub fn light() -> Self {
    Self {
      accent: Color::Rgb(30, 100, 160),
      header: Color::Rgb(20, 70, 130),
      text: Color::Rgb(30, 36, 48),
      text_dim: Color::Rgb(100, 110, 128),
      border: Color::Rgb(148, 162, 178),
      border_active: Color::Rgb(80, 115, 148),
      bg: Color::Rgb(252, 252, 252),
      bg_panel: Color::Rgb(252, 252, 252),
      bg_input: Color::Rgb(232, 238, 245),
      bg_selection: Color::Rgb(200, 218, 236),
      bg_code: Color::Rgb(240, 242, 244),
      bg_chat: Color::Rgb(248, 249, 250),
      bg_user_msg: Color::Rgb(232, 236, 240),
      bg_popup: Color::Rgb(252, 252, 252),
      text_on_accent: Color::White,
      success: Color::Rgb(40, 140, 70),
      warning: Color::Rgb(160, 120, 20),
      error: Color::Rgb(180, 40, 40),
      math: Color::Rgb(35, 135, 110),
      mono: Color::Rgb(130, 105, 55),
      rule: Color::Rgb(150, 160, 172),
      toc_dim: Color::Rgb(105, 118, 136),
      bookmark_bg: Color::Rgb(245, 232, 176),
      cursor_bg: Color::Rgb(30, 36, 48),
      cursor_fg: Color::White,
      search_match_bg: Color::Rgb(255, 225, 90),
      search_match_fg: Color::Black,
    }
  }

  pub fn amoled() -> Self {
    Self {
      accent: Color::Rgb(0, 200, 255),
      header: Color::Rgb(80, 180, 255),
      text: Color::Rgb(230, 235, 240),
      text_dim: Color::Rgb(130, 140, 150),
      border: Color::Rgb(36, 36, 36),
      border_active: Color::Rgb(70, 70, 70),
      bg: Color::Black,
      bg_panel: Color::Black,
      bg_input: Color::Rgb(8, 18, 26),
      bg_selection: Color::Rgb(20, 40, 60),
      bg_code: Color::Black,
      bg_chat: Color::Black,
      bg_user_msg: Color::Rgb(14, 14, 14),
      bg_popup: Color::Black,
      text_on_accent: Color::Black,
      success: Color::Rgb(0, 220, 120),
      warning: Color::Rgb(255, 190, 0),
      error: Color::Rgb(255, 60, 60),
      math: Color::Rgb(0, 230, 170),
      mono: Color::Rgb(210, 180, 120),
      rule: Color::Rgb(45, 55, 68),
      toc_dim: Color::Rgb(92, 104, 118),
      bookmark_bg: Color::Rgb(55, 45, 15),
      cursor_bg: Color::White,
      cursor_fg: Color::Black,
      search_match_bg: Color::Yellow,
      search_match_fg: Color::Black,
    }
  }

  fn solarized_dark() -> Self {
    Self {
      accent: Color::Rgb(38, 139, 210),
      header: Color::Rgb(42, 161, 152),
      text: Color::Rgb(131, 148, 150),
      text_dim: Color::Rgb(88, 110, 117),
      border: Color::Rgb(7, 54, 66),
      border_active: Color::Rgb(88, 110, 117),
      bg: Color::Rgb(0, 43, 54),
      bg_panel: Color::Rgb(7, 54, 66),
      bg_input: Color::Rgb(7, 54, 66),
      bg_selection: Color::Rgb(47, 79, 86),
      bg_code: Color::Rgb(7, 54, 66),
      bg_chat: Color::Rgb(0, 43, 54),
      bg_user_msg: Color::Rgb(7, 54, 66),
      bg_popup: Color::Rgb(0, 43, 54),
      text_on_accent: Color::Rgb(253, 246, 227),
      success: Color::Rgb(133, 153, 0),
      warning: Color::Rgb(181, 137, 0),
      error: Color::Rgb(220, 50, 47),
      math: Color::Rgb(42, 161, 152),
      mono: Color::Rgb(181, 137, 0),
      rule: Color::Rgb(88, 110, 117),
      toc_dim: Color::Rgb(101, 123, 131),
      bookmark_bg: Color::Rgb(73, 63, 18),
      cursor_bg: Color::Rgb(238, 232, 213),
      cursor_fg: Color::Rgb(0, 43, 54),
      search_match_bg: Color::Rgb(181, 137, 0),
      search_match_fg: Color::Rgb(0, 43, 54),
    }
  }

  fn solarized_light() -> Self {
    Self {
      accent: Color::Rgb(38, 139, 210),
      header: Color::Rgb(42, 161, 152),
      text: Color::Rgb(88, 110, 117),
      text_dim: Color::Rgb(101, 123, 131),
      border: Color::Rgb(147, 161, 161),
      border_active: Color::Rgb(101, 123, 131),
      bg: Color::Rgb(253, 246, 227),
      bg_panel: Color::Rgb(238, 232, 213),
      bg_input: Color::Rgb(238, 232, 213),
      bg_selection: Color::Rgb(220, 226, 211),
      bg_code: Color::Rgb(238, 232, 213),
      bg_chat: Color::Rgb(253, 246, 227),
      bg_user_msg: Color::Rgb(238, 232, 213),
      bg_popup: Color::Rgb(253, 246, 227),
      text_on_accent: Color::White,
      success: Color::Rgb(133, 153, 0),
      warning: Color::Rgb(181, 137, 0),
      error: Color::Rgb(220, 50, 47),
      math: Color::Rgb(42, 161, 152),
      mono: Color::Rgb(181, 137, 0),
      rule: Color::Rgb(147, 161, 161),
      toc_dim: Color::Rgb(101, 123, 131),
      bookmark_bg: Color::Rgb(246, 224, 160),
      cursor_bg: Color::Rgb(88, 110, 117),
      cursor_fg: Color::Rgb(253, 246, 227),
      search_match_bg: Color::Rgb(253, 203, 110),
      search_match_fg: Color::Rgb(0, 43, 54),
    }
  }

  fn gruvbox_dark() -> Self {
    Self {
      accent: Color::Rgb(131, 165, 152),
      header: Color::Rgb(250, 189, 47),
      text: Color::Rgb(235, 219, 178),
      text_dim: Color::Rgb(168, 153, 132),
      border: Color::Rgb(80, 73, 69),
      border_active: Color::Rgb(124, 111, 100),
      bg: Color::Rgb(40, 40, 40),
      bg_panel: Color::Rgb(50, 48, 47),
      bg_input: Color::Rgb(60, 56, 54),
      bg_selection: Color::Rgb(80, 73, 69),
      bg_code: Color::Rgb(29, 32, 33),
      bg_chat: Color::Rgb(40, 40, 40),
      bg_user_msg: Color::Rgb(60, 56, 54),
      bg_popup: Color::Rgb(40, 40, 40),
      text_on_accent: Color::Rgb(40, 40, 40),
      success: Color::Rgb(184, 187, 38),
      warning: Color::Rgb(250, 189, 47),
      error: Color::Rgb(251, 73, 52),
      math: Color::Rgb(142, 192, 124),
      mono: Color::Rgb(211, 134, 155),
      rule: Color::Rgb(102, 92, 84),
      toc_dim: Color::Rgb(168, 153, 132),
      bookmark_bg: Color::Rgb(83, 68, 25),
      cursor_bg: Color::Rgb(235, 219, 178),
      cursor_fg: Color::Rgb(40, 40, 40),
      search_match_bg: Color::Rgb(250, 189, 47),
      search_match_fg: Color::Rgb(40, 40, 40),
    }
  }

  fn nord() -> Self {
    Self {
      accent: Color::Rgb(136, 192, 208),
      header: Color::Rgb(129, 161, 193),
      text: Color::Rgb(216, 222, 233),
      text_dim: Color::Rgb(143, 161, 179),
      border: Color::Rgb(67, 76, 94),
      border_active: Color::Rgb(94, 129, 172),
      bg: Color::Rgb(46, 52, 64),
      bg_panel: Color::Rgb(59, 66, 82),
      bg_input: Color::Rgb(67, 76, 94),
      bg_selection: Color::Rgb(76, 86, 106),
      bg_code: Color::Rgb(37, 41, 51),
      bg_chat: Color::Rgb(46, 52, 64),
      bg_user_msg: Color::Rgb(59, 66, 82),
      bg_popup: Color::Rgb(46, 52, 64),
      text_on_accent: Color::Rgb(46, 52, 64),
      success: Color::Rgb(163, 190, 140),
      warning: Color::Rgb(235, 203, 139),
      error: Color::Rgb(191, 97, 106),
      math: Color::Rgb(143, 188, 187),
      mono: Color::Rgb(180, 142, 173),
      rule: Color::Rgb(76, 86, 106),
      toc_dim: Color::Rgb(129, 161, 193),
      bookmark_bg: Color::Rgb(84, 75, 42),
      cursor_bg: Color::Rgb(236, 239, 244),
      cursor_fg: Color::Rgb(46, 52, 64),
      search_match_bg: Color::Rgb(235, 203, 139),
      search_match_fg: Color::Rgb(46, 52, 64),
    }
  }

  fn tokyo_night() -> Self {
    Self {
      accent: Color::Rgb(125, 207, 255),
      header: Color::Rgb(122, 162, 247),
      text: Color::Rgb(192, 202, 245),
      text_dim: Color::Rgb(86, 95, 137),
      border: Color::Rgb(41, 46, 66),
      border_active: Color::Rgb(86, 95, 137),
      bg: Color::Rgb(26, 27, 38),
      bg_panel: Color::Rgb(31, 35, 53),
      bg_input: Color::Rgb(36, 40, 59),
      bg_selection: Color::Rgb(45, 52, 73),
      bg_code: Color::Rgb(22, 22, 30),
      bg_chat: Color::Rgb(26, 27, 38),
      bg_user_msg: Color::Rgb(36, 40, 59),
      bg_popup: Color::Rgb(26, 27, 38),
      text_on_accent: Color::Rgb(26, 27, 38),
      success: Color::Rgb(158, 206, 106),
      warning: Color::Rgb(224, 175, 104),
      error: Color::Rgb(247, 118, 142),
      math: Color::Rgb(115, 218, 202),
      mono: Color::Rgb(187, 154, 247),
      rule: Color::Rgb(65, 72, 104),
      toc_dim: Color::Rgb(122, 162, 247),
      bookmark_bg: Color::Rgb(73, 61, 38),
      cursor_bg: Color::Rgb(192, 202, 245),
      cursor_fg: Color::Rgb(26, 27, 38),
      search_match_bg: Color::Rgb(224, 175, 104),
      search_match_fg: Color::Rgb(26, 27, 38),
    }
  }

  fn catppuccin_mocha() -> Self {
    Self {
      accent: Color::Rgb(137, 180, 250),
      header: Color::Rgb(137, 220, 235),
      text: Color::Rgb(205, 214, 244),
      text_dim: Color::Rgb(147, 153, 178),
      border: Color::Rgb(69, 71, 90),
      border_active: Color::Rgb(116, 122, 162),
      bg: Color::Rgb(30, 30, 46),
      bg_panel: Color::Rgb(24, 24, 37),
      bg_input: Color::Rgb(49, 50, 68),
      bg_selection: Color::Rgb(69, 71, 90),
      bg_code: Color::Rgb(17, 17, 27),
      bg_chat: Color::Rgb(30, 30, 46),
      bg_user_msg: Color::Rgb(49, 50, 68),
      bg_popup: Color::Rgb(30, 30, 46),
      text_on_accent: Color::Rgb(30, 30, 46),
      success: Color::Rgb(166, 227, 161),
      warning: Color::Rgb(249, 226, 175),
      error: Color::Rgb(243, 139, 168),
      math: Color::Rgb(148, 226, 213),
      mono: Color::Rgb(245, 194, 231),
      rule: Color::Rgb(88, 91, 112),
      toc_dim: Color::Rgb(180, 190, 254),
      bookmark_bg: Color::Rgb(74, 67, 42),
      cursor_bg: Color::Rgb(205, 214, 244),
      cursor_fg: Color::Rgb(30, 30, 46),
      search_match_bg: Color::Rgb(249, 226, 175),
      search_match_fg: Color::Rgb(30, 30, 46),
    }
  }

  fn powder_blue() -> Self {
    powder(
      Color::Rgb(236, 244, 248),
      Color::Rgb(215, 231, 239),
      Color::Rgb(57, 91, 112),
      Color::Rgb(84, 128, 151),
      Color::Rgb(111, 160, 183),
    )
  }

  fn powder_sage() -> Self {
    powder(
      Color::Rgb(238, 244, 236),
      Color::Rgb(220, 233, 216),
      Color::Rgb(62, 88, 69),
      Color::Rgb(86, 122, 92),
      Color::Rgb(124, 157, 116),
    )
  }

  fn powder_lavender() -> Self {
    powder(
      Color::Rgb(243, 239, 248),
      Color::Rgb(228, 220, 239),
      Color::Rgb(78, 67, 103),
      Color::Rgb(117, 96, 151),
      Color::Rgb(156, 132, 188),
    )
  }

  fn powder_rose() -> Self {
    powder(
      Color::Rgb(249, 239, 241),
      Color::Rgb(239, 220, 225),
      Color::Rgb(103, 66, 75),
      Color::Rgb(151, 92, 106),
      Color::Rgb(190, 124, 139),
    )
  }

  fn powder_mint() -> Self {
    powder(
      Color::Rgb(235, 246, 241),
      Color::Rgb(214, 235, 225),
      Color::Rgb(53, 92, 79),
      Color::Rgb(76, 132, 111),
      Color::Rgb(111, 177, 149),
    )
  }

  fn powder_sand() -> Self {
    powder(
      Color::Rgb(247, 243, 233),
      Color::Rgb(235, 226, 207),
      Color::Rgb(94, 78, 52),
      Color::Rgb(137, 111, 69),
      Color::Rgb(178, 145, 88),
    )
  }

  fn powder_slate() -> Self {
    powder(
      Color::Rgb(237, 241, 244),
      Color::Rgb(219, 226, 232),
      Color::Rgb(56, 70, 84),
      Color::Rgb(86, 105, 123),
      Color::Rgb(117, 139, 158),
    )
  }

  pub fn style_default(&self) -> Style {
    Style::default().fg(self.text)
  }

  pub fn style_dim(&self) -> Style {
    Style::default().fg(self.text_dim)
  }

  pub fn style_accent(&self) -> Style {
    Style::default().fg(self.accent)
  }

  pub fn style_header(&self) -> Style {
    Style::default().fg(self.header).add_modifier(Modifier::BOLD)
  }

  pub fn style_border(&self) -> Style {
    Style::default().fg(self.border)
  }

  pub fn style_border_active(&self) -> Style {
    Style::default().fg(self.border_active)
  }

  pub fn style_selection(&self) -> Style {
    Style::default().bg(self.bg_selection).fg(self.text)
  }

  pub fn style_selection_text(&self) -> Style {
    Style::default()
      .bg(self.bg_selection)
      .fg(self.text)
      .add_modifier(Modifier::BOLD)
  }

  pub fn style_selection_dim(&self) -> Style {
    Style::default().bg(self.bg_selection).fg(self.text)
  }

  pub fn style_success(&self) -> Style {
    Style::default().fg(self.success)
  }

  pub fn style_warning(&self) -> Style {
    Style::default().fg(self.warning)
  }

  pub fn style_error(&self) -> Style {
    Style::default().fg(self.error)
  }
}

fn powder(
  bg: Color,
  panel: Color,
  text: Color,
  accent: Color,
  soft: Color,
) -> Theme {
  Theme {
    accent,
    header: accent,
    text,
    text_dim: mix(text, bg, 58),
    border: mix(text, bg, 72),
    border_active: accent,
    bg,
    bg_panel: panel,
    bg_input: panel,
    bg_selection: mix(soft, bg, 70),
    bg_code: mix(panel, text, 88),
    bg_chat: bg,
    bg_user_msg: panel,
    bg_popup: bg,
    text_on_accent: Color::White,
    success: Color::Rgb(83, 133, 92),
    warning: Color::Rgb(160, 125, 62),
    error: Color::Rgb(168, 78, 84),
    math: Color::Rgb(73, 135, 122),
    mono: Color::Rgb(136, 104, 74),
    rule: mix(text, bg, 78),
    toc_dim: mix(text, bg, 50),
    bookmark_bg: Color::Rgb(241, 229, 185),
    cursor_bg: text,
    cursor_fg: bg,
    search_match_bg: Color::Rgb(246, 217, 133),
    search_match_fg: text,
  }
}

fn mix(a: Color, b: Color, b_pct: u8) -> Color {
  let (ar, ag, ab) = rgb(a);
  let (br, bg, bb) = rgb(b);
  let bp = b_pct as u16;
  let ap = 100u16.saturating_sub(bp);
  Color::Rgb(
    ((ar as u16 * ap + br as u16 * bp) / 100) as u8,
    ((ag as u16 * ap + bg as u16 * bp) / 100) as u8,
    ((ab as u16 * ap + bb as u16 * bp) / 100) as u8,
  )
}

fn rgb(color: Color) -> (u8, u8, u8) {
  match color {
    Color::Black => (0, 0, 0),
    Color::White => (255, 255, 255),
    Color::Red => (205, 49, 49),
    Color::Green => (13, 188, 121),
    Color::Yellow => (229, 229, 16),
    Color::Blue => (36, 114, 200),
    Color::Magenta => (188, 63, 188),
    Color::Cyan => (17, 168, 205),
    Color::Gray => (128, 128, 128),
    Color::DarkGray => (88, 88, 88),
    Color::LightRed => (241, 76, 76),
    Color::LightGreen => (35, 209, 139),
    Color::LightYellow => (245, 245, 67),
    Color::LightBlue => (59, 142, 234),
    Color::LightMagenta => (214, 112, 214),
    Color::LightCyan => (41, 184, 219),
    Color::Rgb(r, g, b) => (r, g, b),
    Color::Indexed(_) | Color::Reset => (0, 0, 0),
  }
}

pub const ACCENT: Color = Color::Rgb(135, 206, 235);
pub const HEADER: Color = Color::Rgb(62, 126, 180);
pub const TEXT: Color = Color::Rgb(218, 224, 230);
pub const TEXT_DIM: Color = Color::Rgb(105, 118, 130);
pub const BORDER: Color = Color::Rgb(48, 60, 72);
pub const BORDER_ACTIVE: Color = Color::Rgb(80, 96, 112);
pub const BG_SELECTION: Color = Color::Rgb(58, 74, 90);
pub const BG_CODE: Color = Color::Rgb(28, 28, 28);
pub const BG_CHAT: Color = Color::Rgb(18, 18, 18);
pub const BG_USER_MSG: Color = Color::Rgb(35, 35, 35);
pub const BG_POPUP: Color = Color::Black;
pub const TEXT_ON_ACCENT: Color = Color::Black;
pub const SUCCESS: Color = Color::Rgb(132, 190, 145);
pub const WARNING: Color = Color::Rgb(204, 180, 105);
pub const ERROR: Color = Color::Red;
pub const INFO: Color = ACCENT;

pub fn style_default() -> Style {
  Theme::dark().style_default()
}

pub fn style_dim() -> Style {
  Theme::dark().style_dim()
}

pub fn style_accent() -> Style {
  Theme::dark().style_accent()
}

pub fn style_header() -> Style {
  Theme::dark().style_header()
}

pub fn style_border() -> Style {
  Theme::dark().style_border()
}

pub fn style_border_active() -> Style {
  Theme::dark().style_border_active()
}

pub fn style_selection() -> Style {
  Theme::dark().style_selection()
}

pub fn style_success() -> Style {
  Theme::dark().style_success()
}

pub fn style_warning() -> Style {
  Theme::dark().style_warning()
}

pub fn style_error() -> Style {
  Theme::dark().style_error()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn theme_ids_round_trip_as_kebab_case() {
    for id in ThemeId::all() {
      let json = serde_json::to_string(id).unwrap();
      assert_eq!(json, format!("\"{}\"", id.info().id));
      let decoded: ThemeId = serde_json::from_str(&json).unwrap();
      assert_eq!(decoded, *id);
    }
  }

  #[test]
  fn legacy_theme_names_deserialize() {
    assert_eq!(
      serde_json::from_str::<ThemeId>("\"Dark\"").unwrap(),
      ThemeId::Dark
    );
    assert_eq!(
      serde_json::from_str::<ThemeId>("\"Light\"").unwrap(),
      ThemeId::Light
    );
    assert_eq!(
      serde_json::from_str::<ThemeId>("\"Amoled\"").unwrap(),
      ThemeId::Amoled
    );
  }

  #[test]
  fn every_theme_has_metadata() {
    for id in ThemeId::all() {
      let info = id.info();
      assert!(!info.id.is_empty());
      assert!(!info.name.is_empty());
    }
  }

  #[test]
  fn every_theme_has_distinct_core_colors() {
    for id in ThemeId::all() {
      let t = id.theme();
      assert_ne!(t.text, t.bg, "{id:?} text/bg");
      assert_ne!(t.accent, t.bg, "{id:?} accent/bg");
      assert_ne!(t.bg_selection, t.bg, "{id:?} selection/bg");
    }
  }

  #[test]
  fn every_theme_selection_helpers_keep_text_visible() {
    for id in ThemeId::all() {
      let t = id.theme();
      assert_ne!(
        t.style_selection_text().fg,
        Some(t.bg_selection),
        "{id:?} selection text/bg"
      );
      assert_ne!(
        t.style_selection_dim().fg,
        Some(t.bg_selection),
        "{id:?} selection dim/bg"
      );
    }
  }
}
