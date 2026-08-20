//! The complete framework-independent Vague Pro theme used by SpaceTerm.
//!
//! Every color from the canonical Vague Pro palette is represented here, including
//! currently unused syntax, collaboration, editor, and status colors. GPUI and
//! libghostty-vt adapters must consume these tokens rather than define colors.
//!
//! Palette source: <https://github.com/sadiksaifi/vague-pro-zed>.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Color {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

impl Color {
    pub(crate) const fn rgb(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
            a: 0xff,
        }
    }

    pub(crate) const fn rgba(hex: u32) -> Self {
        Self {
            r: ((hex >> 24) & 0xff) as u8,
            g: ((hex >> 16) & 0xff) as u8,
            b: ((hex >> 8) & 0xff) as u8,
            a: (hex & 0xff) as u8,
        }
    }

    pub(crate) const fn from_rgb_components(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    pub(crate) const fn rgba_hex(self) -> u32 {
        (self.r as u32) << 24 | (self.g as u32) << 16 | (self.b as u32) << 8 | self.a as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontStyle {
    Italic,
}

#[expect(
    dead_code,
    reason = "syntax roles are canonical even before SpaceTerm renders them"
)]
pub(crate) struct SyntaxStyle {
    pub(crate) color: Color,
    pub(crate) font_weight: Option<u16>,
    pub(crate) font_style: Option<FontStyle>,
}

#[expect(
    dead_code,
    reason = "the complete Vague Pro syntax palette is retained for future consumers"
)]
pub(crate) struct SyntaxTheme {
    pub(crate) comment: SyntaxStyle,
    pub(crate) comment_doc: SyntaxStyle,
    pub(crate) string: SyntaxStyle,
    pub(crate) string_special: SyntaxStyle,
    pub(crate) string_special_symbol: SyntaxStyle,
    pub(crate) string_escape: SyntaxStyle,
    pub(crate) string_regex: SyntaxStyle,
    pub(crate) link_text: SyntaxStyle,
    pub(crate) link_uri: SyntaxStyle,
    pub(crate) text_literal: SyntaxStyle,
    pub(crate) number: SyntaxStyle,
    pub(crate) boolean: SyntaxStyle,
    pub(crate) constant: SyntaxStyle,
    pub(crate) constant_builtin: SyntaxStyle,
    pub(crate) character: SyntaxStyle,
    pub(crate) character_special: SyntaxStyle,
    pub(crate) variable: SyntaxStyle,
    pub(crate) variable_parameter: SyntaxStyle,
    pub(crate) variable_member: SyntaxStyle,
    pub(crate) variable_special: SyntaxStyle,
    pub(crate) property: SyntaxStyle,
    pub(crate) function: SyntaxStyle,
    pub(crate) function_call: SyntaxStyle,
    pub(crate) function_macro: SyntaxStyle,
    pub(crate) function_method: SyntaxStyle,
    pub(crate) function_method_call: SyntaxStyle,
    pub(crate) keyword: SyntaxStyle,
    pub(crate) keyword_control: SyntaxStyle,
    pub(crate) keyword_operator_regex: SyntaxStyle,
    pub(crate) label: SyntaxStyle,
    pub(crate) title: SyntaxStyle,
    pub(crate) operator: SyntaxStyle,
    pub(crate) preproc: SyntaxStyle,
    pub(crate) constructor: SyntaxStyle,
    pub(crate) module: SyntaxStyle,
    pub(crate) builtin: SyntaxStyle,
    pub(crate) hint: SyntaxStyle,
    pub(crate) type_: SyntaxStyle,
    pub(crate) type_builtin: SyntaxStyle,
    pub(crate) type_class: SyntaxStyle,
    pub(crate) enum_: SyntaxStyle,
    pub(crate) namespace: SyntaxStyle,
    pub(crate) variant: SyntaxStyle,
    pub(crate) tag: SyntaxStyle,
    pub(crate) tag_component_jsx: SyntaxStyle,
    pub(crate) tag_doctype: SyntaxStyle,
    pub(crate) tag_attribute: SyntaxStyle,
    pub(crate) tag_delimiter: SyntaxStyle,
    pub(crate) attribute: SyntaxStyle,
    pub(crate) attribute_builtin: SyntaxStyle,
    pub(crate) attribute_jsx: SyntaxStyle,
    pub(crate) punctuation: SyntaxStyle,
    pub(crate) punctuation_special: SyntaxStyle,
    pub(crate) punctuation_delimiter: SyntaxStyle,
    pub(crate) punctuation_bracket: SyntaxStyle,
    pub(crate) punctuation_list_marker: SyntaxStyle,
    pub(crate) punctuation_markup: SyntaxStyle,
    pub(crate) text_jsx: SyntaxStyle,
    pub(crate) emphasis: SyntaxStyle,
    pub(crate) emphasis_strong: SyntaxStyle,
    pub(crate) embedded: SyntaxStyle,
    pub(crate) primary: SyntaxStyle,
    pub(crate) predictive: SyntaxStyle,
    pub(crate) selector: SyntaxStyle,
    pub(crate) selector_pseudo: SyntaxStyle,
    pub(crate) diff_plus: SyntaxStyle,
    pub(crate) diff_minus: SyntaxStyle,
}

#[expect(
    dead_code,
    reason = "player colors are canonical even before collaboration UI exists"
)]
pub(crate) struct PlayerTheme {
    pub(crate) background: Color,
    pub(crate) cursor: Color,
    pub(crate) selection: Color,
}

#[expect(
    dead_code,
    reason = "all canonical Vague Pro roles are defined before every UI consumer exists"
)]
pub(crate) struct Theme {
    pub(crate) accents: [Color; 7],
    pub(crate) syntax: SyntaxTheme,
    pub(crate) players: [PlayerTheme; 8],
    pub(crate) background: Color,
    pub(crate) link_text_hover: Color,
    pub(crate) error: Color,
    pub(crate) error_border: Color,
    pub(crate) error_background: Color,
    pub(crate) warning: Color,
    pub(crate) warning_border: Color,
    pub(crate) warning_background: Color,
    pub(crate) hint: Color,
    pub(crate) hint_border: Color,
    pub(crate) hint_background: Color,
    pub(crate) hidden: Color,
    pub(crate) hidden_border: Color,
    pub(crate) hidden_background: Color,
    pub(crate) ignored: Color,
    pub(crate) ignored_border: Color,
    pub(crate) ignored_background: Color,
    pub(crate) success: Color,
    pub(crate) success_border: Color,
    pub(crate) success_background: Color,
    pub(crate) conflict: Color,
    pub(crate) conflict_border: Color,
    pub(crate) conflict_background: Color,
    pub(crate) created: Color,
    pub(crate) created_border: Color,
    pub(crate) created_background: Color,
    pub(crate) modified: Color,
    pub(crate) modified_border: Color,
    pub(crate) modified_background: Color,
    pub(crate) deleted: Color,
    pub(crate) deleted_border: Color,
    pub(crate) deleted_background: Color,
    pub(crate) version_control_added: Color,
    pub(crate) version_control_modified: Color,
    pub(crate) version_control_deleted: Color,
    pub(crate) version_control_conflict_marker_ours: Color,
    pub(crate) version_control_conflict_marker_theirs: Color,
    pub(crate) unreachable: Color,
    pub(crate) unreachable_border: Color,
    pub(crate) unreachable_background: Color,
    pub(crate) info: Color,
    pub(crate) info_border: Color,
    pub(crate) info_background: Color,
    pub(crate) predictive: Color,
    pub(crate) predictive_border: Color,
    pub(crate) predictive_background: Color,
    pub(crate) renamed: Color,
    pub(crate) renamed_border: Color,
    pub(crate) renamed_background: Color,
    pub(crate) status_bar_foreground: Color,
    pub(crate) text: Color,
    pub(crate) text_accent: Color,
    pub(crate) text_disabled: Color,
    pub(crate) text_muted: Color,
    pub(crate) text_placeholder: Color,
    pub(crate) element_background: Color,
    pub(crate) element_active: Color,
    pub(crate) element_disabled: Color,
    pub(crate) element_hover: Color,
    pub(crate) element_selected: Color,
    pub(crate) ghost_element_background: Color,
    pub(crate) ghost_element_disabled: Color,
    pub(crate) ghost_element_hover: Color,
    pub(crate) ghost_element_active: Color,
    pub(crate) ghost_element_selected: Color,
    pub(crate) icon_accent: Color,
    pub(crate) icon_muted: Color,
    pub(crate) icon: Color,
    pub(crate) icon_disabled: Color,
    pub(crate) icon_placeholder: Color,
    pub(crate) debugger_accent: Color,
    pub(crate) scrollbar_thumb_background: Color,
    pub(crate) scrollbar_thumb_hover_background: Color,
    pub(crate) scrollbar_track_border: Color,
    pub(crate) scrollbar_thumb_border: Color,
    pub(crate) drop_target_background: Color,
    pub(crate) editor_active_line_background: Color,
    pub(crate) editor_active_line_number: Color,
    pub(crate) editor_active_wrap_guide: Color,
    pub(crate) editor_background: Color,
    pub(crate) editor_foreground: Color,
    pub(crate) editor_gutter_background: Color,
    pub(crate) editor_line_number: Color,
    pub(crate) editor_highlighted_line_background: Color,
    pub(crate) editor_invisible: Color,
    pub(crate) editor_subheader_background: Color,
    pub(crate) editor_wrap_guide: Color,
    pub(crate) editor_document_highlight_bracket_background: Color,
    pub(crate) editor_document_highlight_read_background: Color,
    pub(crate) editor_document_highlight_write_background: Color,
    pub(crate) editor_indent_guide: Color,
    pub(crate) editor_indent_guide_active: Color,
    pub(crate) elevated_surface_background: Color,
    pub(crate) panel_background: Color,
    pub(crate) panel_focused_border: Color,
    pub(crate) panel_indent_guide: Color,
    pub(crate) panel_indent_guide_hover: Color,
    pub(crate) panel_indent_guide_active: Color,
    pub(crate) search_match_background: Color,
    pub(crate) search_current_match_background: Color,
    pub(crate) status_bar_background: Color,
    pub(crate) surface_background: Color,
    pub(crate) tab_active_background: Color,
    pub(crate) tab_inactive_background: Color,
    pub(crate) tab_bar_background: Color,
    pub(crate) title_bar_background: Color,
    pub(crate) title_bar_inactive_background: Color,
    pub(crate) toolbar_background: Color,
    pub(crate) border: Color,
    pub(crate) border_variant: Color,
    pub(crate) border_selected: Color,
    pub(crate) border_disabled: Color,
    pub(crate) border_focused: Color,
    pub(crate) border_transparent: Color,
    pub(crate) terminal_ansi_black: Color,
    pub(crate) terminal_ansi_bright_black: Color,
    pub(crate) terminal_ansi_dim_black: Color,
    pub(crate) terminal_ansi_red: Color,
    pub(crate) terminal_ansi_bright_red: Color,
    pub(crate) terminal_ansi_dim_red: Color,
    pub(crate) terminal_ansi_green: Color,
    pub(crate) terminal_ansi_bright_green: Color,
    pub(crate) terminal_ansi_dim_green: Color,
    pub(crate) terminal_ansi_yellow: Color,
    pub(crate) terminal_ansi_dim_yellow: Color,
    pub(crate) terminal_ansi_bright_yellow: Color,
    pub(crate) terminal_ansi_blue: Color,
    pub(crate) terminal_ansi_bright_blue: Color,
    pub(crate) terminal_ansi_dim_blue: Color,
    pub(crate) terminal_ansi_magenta: Color,
    pub(crate) terminal_ansi_bright_magenta: Color,
    pub(crate) terminal_ansi_dim_magenta: Color,
    pub(crate) terminal_ansi_cyan: Color,
    pub(crate) terminal_ansi_bright_cyan: Color,
    pub(crate) terminal_ansi_dim_cyan: Color,
    pub(crate) terminal_ansi_white: Color,
    pub(crate) terminal_ansi_bright_white: Color,
    pub(crate) terminal_ansi_dim_white: Color,
    pub(crate) terminal_background: Color,
    pub(crate) terminal_foreground: Color,
    pub(crate) terminal_bright_foreground: Color,
    pub(crate) terminal_dim_foreground: Color,
}

impl Theme {
    pub(crate) const fn terminal_normal(&self) -> [Color; 8] {
        [
            self.terminal_ansi_black,
            self.terminal_ansi_red,
            self.terminal_ansi_green,
            self.terminal_ansi_yellow,
            self.terminal_ansi_blue,
            self.terminal_ansi_magenta,
            self.terminal_ansi_cyan,
            self.terminal_ansi_white,
        ]
    }

    pub(crate) const fn terminal_bright(&self) -> [Color; 8] {
        [
            self.terminal_ansi_bright_black,
            self.terminal_ansi_bright_red,
            self.terminal_ansi_bright_green,
            self.terminal_ansi_bright_yellow,
            self.terminal_ansi_bright_blue,
            self.terminal_ansi_bright_magenta,
            self.terminal_ansi_bright_cyan,
            self.terminal_ansi_bright_white,
        ]
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dim palette is complete before faint text rendering consumes it"
        )
    )]
    pub(crate) const fn terminal_dim(&self) -> [Color; 8] {
        [
            self.terminal_ansi_dim_black,
            self.terminal_ansi_dim_red,
            self.terminal_ansi_dim_green,
            self.terminal_ansi_dim_yellow,
            self.terminal_ansi_dim_blue,
            self.terminal_ansi_dim_magenta,
            self.terminal_ansi_dim_cyan,
            self.terminal_ansi_dim_white,
        ]
    }
}

pub(crate) const ACTIVE_THEME: &Theme = &VAGUE_PRO;

pub(crate) static VAGUE_PRO: Theme = Theme {
    accents: [
        Color::rgb(0x7e_98_e8),
        Color::rgb(0x6e_94_b2),
        Color::rgb(0x7f_a5_63),
        Color::rgb(0xe8_b5_89),
        Color::rgb(0xc4_82_82),
        Color::rgb(0xbb_9b_db),
        Color::rgb(0xae_ae_d1),
    ],
    syntax: SyntaxTheme {
        comment: SyntaxStyle {
            color: Color::rgb(0x60_60_79),
            font_weight: None,
            font_style: None,
        },
        comment_doc: SyntaxStyle {
            color: Color::rgb(0x87_87_87),
            font_weight: None,
            font_style: Some(FontStyle::Italic),
        },
        string: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        string_special: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        string_special_symbol: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: None,
            font_style: None,
        },
        string_escape: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        string_regex: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        link_text: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        link_uri: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        text_literal: SyntaxStyle {
            color: Color::rgb(0xae_ae_d1),
            font_weight: None,
            font_style: None,
        },
        number: SyntaxStyle {
            color: Color::rgb(0xe0_a3_63),
            font_weight: None,
            font_style: None,
        },
        boolean: SyntaxStyle {
            color: Color::rgb(0xe0_a3_63),
            font_weight: Some(700),
            font_style: None,
        },
        constant: SyntaxStyle {
            color: Color::rgb(0xae_ae_d1),
            font_weight: None,
            font_style: None,
        },
        constant_builtin: SyntaxStyle {
            color: Color::rgb(0xe0_a3_63),
            font_weight: Some(700),
            font_style: None,
        },
        character: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        character_special: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        variable: SyntaxStyle {
            color: Color::rgb(0xae_ae_d1),
            font_weight: None,
            font_style: None,
        },
        variable_parameter: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        variable_member: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        variable_special: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: None,
            font_style: None,
        },
        property: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        function: SyntaxStyle {
            color: Color::rgb(0xc4_82_82),
            font_weight: None,
            font_style: None,
        },
        function_call: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        function_macro: SyntaxStyle {
            color: Color::rgb(0x7f_a5_63),
            font_weight: None,
            font_style: None,
        },
        function_method: SyntaxStyle {
            color: Color::rgb(0xc4_82_82),
            font_weight: None,
            font_style: None,
        },
        function_method_call: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        keyword: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        keyword_control: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        keyword_operator_regex: SyntaxStyle {
            color: Color::rgb(0x90_a0_b5),
            font_weight: None,
            font_style: None,
        },
        label: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        title: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        operator: SyntaxStyle {
            color: Color::rgb(0x90_a0_b5),
            font_weight: None,
            font_style: None,
        },
        preproc: SyntaxStyle {
            color: Color::rgb(0xae_ae_d1),
            font_weight: None,
            font_style: None,
        },
        constructor: SyntaxStyle {
            color: Color::rgb(0xe8_b5_89),
            font_weight: None,
            font_style: None,
        },
        module: SyntaxStyle {
            color: Color::rgb(0xae_ae_d1),
            font_weight: None,
            font_style: None,
        },
        builtin: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: None,
            font_style: None,
        },
        hint: SyntaxStyle {
            color: Color::rgb(0x7e_98_e8),
            font_weight: None,
            font_style: None,
        },
        type_: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        type_builtin: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: Some(700),
            font_style: None,
        },
        type_class: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: None,
            font_style: None,
        },
        enum_: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        namespace: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        variant: SyntaxStyle {
            color: Color::rgb(0x7f_a5_63),
            font_weight: None,
            font_style: None,
        },
        tag: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        tag_component_jsx: SyntaxStyle {
            color: Color::rgb(0xb4_d4_cf),
            font_weight: None,
            font_style: None,
        },
        tag_doctype: SyntaxStyle {
            color: Color::rgb(0xe0_a3_63),
            font_weight: None,
            font_style: None,
        },
        tag_attribute: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        tag_delimiter: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        attribute: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        attribute_builtin: SyntaxStyle {
            color: Color::rgb(0x9b_b4_bc),
            font_weight: None,
            font_style: None,
        },
        attribute_jsx: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        punctuation: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        punctuation_special: SyntaxStyle {
            color: Color::rgb(0x6e_94_b2),
            font_weight: None,
            font_style: None,
        },
        punctuation_delimiter: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        punctuation_bracket: SyntaxStyle {
            color: Color::rgb(0x90_a0_b5),
            font_weight: None,
            font_style: None,
        },
        punctuation_list_marker: SyntaxStyle {
            color: Color::rgb(0xc4_82_82),
            font_weight: None,
            font_style: None,
        },
        punctuation_markup: SyntaxStyle {
            color: Color::rgb(0xc4_82_82),
            font_weight: None,
            font_style: None,
        },
        text_jsx: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        emphasis: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: Some(FontStyle::Italic),
        },
        emphasis_strong: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: Some(700),
            font_style: None,
        },
        embedded: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        primary: SyntaxStyle {
            color: Color::rgb(0xcd_cd_cd),
            font_weight: None,
            font_style: None,
        },
        predictive: SyntaxStyle {
            color: Color::rgb(0x60_60_79),
            font_weight: None,
            font_style: Some(FontStyle::Italic),
        },
        selector: SyntaxStyle {
            color: Color::rgb(0x7f_a5_63),
            font_weight: None,
            font_style: None,
        },
        selector_pseudo: SyntaxStyle {
            color: Color::rgb(0xbb_9d_bd),
            font_weight: None,
            font_style: None,
        },
        diff_plus: SyntaxStyle {
            color: Color::rgb(0x7f_a5_63),
            font_weight: None,
            font_style: None,
        },
        diff_minus: SyntaxStyle {
            color: Color::rgb(0xd8_64_7e),
            font_weight: None,
            font_style: None,
        },
    },
    players: [
        PlayerTheme {
            background: Color::rgb(0xcd_cd_cd),
            cursor: Color::rgb(0xcd_cd_cd),
            selection: Color::rgba(0x33_37_38_aa),
        },
        PlayerTheme {
            background: Color::rgb(0x7e_98_e8),
            cursor: Color::rgb(0x7e_98_e8),
            selection: Color::rgba(0x7e_98_e8_44),
        },
        PlayerTheme {
            background: Color::rgb(0x6e_94_b2),
            cursor: Color::rgb(0x6e_94_b2),
            selection: Color::rgba(0x6e_94_b2_44),
        },
        PlayerTheme {
            background: Color::rgb(0x7f_a5_63),
            cursor: Color::rgb(0x7f_a5_63),
            selection: Color::rgba(0x7f_a5_63_44),
        },
        PlayerTheme {
            background: Color::rgb(0xe8_b5_89),
            cursor: Color::rgb(0xe8_b5_89),
            selection: Color::rgba(0xe8_b5_89_44),
        },
        PlayerTheme {
            background: Color::rgb(0xc4_82_82),
            cursor: Color::rgb(0xc4_82_82),
            selection: Color::rgba(0xc4_82_82_44),
        },
        PlayerTheme {
            background: Color::rgb(0xbb_9b_db),
            cursor: Color::rgb(0xbb_9b_db),
            selection: Color::rgba(0xbb_9b_db_44),
        },
        PlayerTheme {
            background: Color::rgb(0xae_ae_d1),
            cursor: Color::rgb(0xae_ae_d1),
            selection: Color::rgba(0xae_ae_d1_44),
        },
    ],
    background: Color::rgb(0x14_14_15),
    link_text_hover: Color::rgb(0x7e_98_e8),
    error: Color::rgb(0xd8_64_7e),
    error_border: Color::rgb(0xd8_64_7e),
    error_background: Color::rgba(0xd8_64_7e_1a),
    warning: Color::rgb(0xf3_be_7c),
    warning_border: Color::rgb(0xf3_be_7c),
    warning_background: Color::rgba(0xf3_be_7c_1a),
    hint: Color::rgb(0x7e_98_e8),
    hint_border: Color::rgb(0x7e_98_e8),
    hint_background: Color::rgba(0x7e_98_e8_1a),
    hidden: Color::rgb(0x60_60_79),
    hidden_border: Color::rgb(0x60_60_79),
    hidden_background: Color::rgba(0x60_60_79_1a),
    ignored: Color::rgb(0x60_60_79),
    ignored_border: Color::rgb(0x60_60_79),
    ignored_background: Color::rgba(0x60_60_79_1a),
    success: Color::rgb(0x7f_a5_63),
    success_border: Color::rgb(0x7f_a5_63),
    success_background: Color::rgba(0x7f_a5_63_1a),
    conflict: Color::rgb(0xf3_be_7c),
    conflict_border: Color::rgb(0xf3_be_7c),
    conflict_background: Color::rgba(0xf3_be_7c_1a),
    created: Color::rgb(0x7f_a5_63),
    created_border: Color::rgb(0x7f_a5_63),
    created_background: Color::rgba(0x7f_a5_63_1a),
    modified: Color::rgb(0xf3_be_7c),
    modified_border: Color::rgb(0xf3_be_7c),
    modified_background: Color::rgba(0xf3_be_7c_1a),
    deleted: Color::rgb(0xd8_64_7e),
    deleted_border: Color::rgb(0xd8_64_7e),
    deleted_background: Color::rgba(0xd8_64_7e_1a),
    version_control_added: Color::rgb(0x7f_a5_63),
    version_control_modified: Color::rgb(0xf3_be_7c),
    version_control_deleted: Color::rgb(0xd8_64_7e),
    version_control_conflict_marker_ours: Color::rgba(0xf3_be_7c_33),
    version_control_conflict_marker_theirs: Color::rgba(0x9b_b4_bc_33),
    unreachable: Color::rgb(0x60_60_79),
    unreachable_border: Color::rgb(0x60_60_79),
    unreachable_background: Color::rgba(0x60_60_79_1a),
    info: Color::rgb(0x7e_98_e8),
    info_border: Color::rgb(0x7e_98_e8),
    info_background: Color::rgba(0x7e_98_e8_1a),
    predictive: Color::rgb(0x60_60_79),
    predictive_border: Color::rgb(0x60_60_79),
    predictive_background: Color::rgba(0x60_60_79_1a),
    renamed: Color::rgb(0xbb_9d_bd),
    renamed_border: Color::rgb(0xbb_9d_bd),
    renamed_background: Color::rgba(0xbb_9d_bd_1a),
    status_bar_foreground: Color::rgb(0xcd_cd_cd),
    text: Color::rgb(0xcd_cd_cd),
    text_accent: Color::rgb(0x6e_94_b2),
    text_disabled: Color::rgb(0x60_60_79),
    text_muted: Color::rgb(0x87_87_87),
    text_placeholder: Color::rgb(0x60_60_79),
    element_background: Color::rgb(0x14_14_15),
    element_active: Color::rgb(0x25_25_30),
    element_disabled: Color::rgb(0x14_14_15),
    element_hover: Color::rgb(0x25_25_30),
    element_selected: Color::rgb(0x25_25_30),
    ghost_element_background: Color::rgba(0x00_00_00_00),
    ghost_element_disabled: Color::rgb(0x14_14_15),
    ghost_element_hover: Color::rgb(0x25_25_30),
    ghost_element_active: Color::rgb(0x25_25_30),
    ghost_element_selected: Color::rgb(0x25_25_30),
    icon_accent: Color::rgb(0x6e_94_b2),
    icon_muted: Color::rgb(0x60_60_79),
    icon: Color::rgb(0xcd_cd_cd),
    icon_disabled: Color::rgb(0x60_60_79),
    icon_placeholder: Color::rgb(0xc3_c3_d5),
    debugger_accent: Color::rgb(0xd8_64_7e),
    scrollbar_thumb_background: Color::rgba(0x33_37_38_78),
    scrollbar_thumb_hover_background: Color::rgba(0x60_60_79_78),
    scrollbar_track_border: Color::rgba(0x00_00_00_00),
    scrollbar_thumb_border: Color::rgba(0x00_00_00_00),
    drop_target_background: Color::rgba(0x40_50_65_80),
    editor_active_line_background: Color::rgb(0x25_25_30),
    editor_active_line_number: Color::rgb(0xcd_cd_cd),
    editor_active_wrap_guide: Color::rgba(0x25_25_30_1a),
    editor_background: Color::rgb(0x14_14_15),
    editor_foreground: Color::rgb(0xcd_cd_cd),
    editor_gutter_background: Color::rgb(0x14_14_15),
    editor_line_number: Color::rgb(0x60_60_79),
    editor_highlighted_line_background: Color::rgb(0x25_25_30),
    editor_invisible: Color::rgb(0x60_60_79),
    editor_subheader_background: Color::rgb(0x14_14_15),
    editor_wrap_guide: Color::rgba(0x25_25_30_0d),
    editor_document_highlight_bracket_background: Color::rgba(0x33_37_38_aa),
    editor_document_highlight_read_background: Color::rgba(0x33_37_38_aa),
    editor_document_highlight_write_background: Color::rgba(0x33_37_38_aa),
    editor_indent_guide: Color::rgb(0x25_25_30),
    editor_indent_guide_active: Color::rgb(0x60_60_79),
    elevated_surface_background: Color::rgb(0x14_14_15),
    panel_background: Color::rgb(0x14_14_15),
    panel_focused_border: Color::rgb(0x6e_94_b2),
    panel_indent_guide: Color::rgb(0x25_25_30),
    panel_indent_guide_hover: Color::rgb(0x40_50_65),
    panel_indent_guide_active: Color::rgb(0x60_60_79),
    search_match_background: Color::rgba(0x6e_94_b2_66),
    search_current_match_background: Color::rgba(0xe8_b5_89_66),
    status_bar_background: Color::rgb(0x14_14_15),
    surface_background: Color::rgb(0x14_14_15),
    tab_active_background: Color::rgb(0x25_25_30),
    tab_inactive_background: Color::rgb(0x14_14_15),
    tab_bar_background: Color::rgb(0x14_14_15),
    title_bar_background: Color::rgb(0x14_14_15),
    title_bar_inactive_background: Color::rgb(0x1c_1c_24),
    toolbar_background: Color::rgb(0x14_14_15),
    border: Color::rgb(0x25_25_30),
    border_variant: Color::rgb(0x25_25_30),
    border_selected: Color::rgb(0x6e_94_b2),
    border_disabled: Color::rgb(0x25_25_30),
    border_focused: Color::rgb(0x40_50_65),
    border_transparent: Color::rgba(0x00_00_00_00),
    terminal_ansi_black: Color::rgb(0x25_25_30),
    terminal_ansi_bright_black: Color::rgb(0x60_60_79),
    terminal_ansi_dim_black: Color::rgb(0x18_18_1f),
    terminal_ansi_red: Color::rgb(0xd8_64_7e),
    terminal_ansi_bright_red: Color::rgb(0xe0_83_98),
    terminal_ansi_dim_red: Color::rgb(0x8e_42_53),
    terminal_ansi_green: Color::rgb(0x7f_a5_63),
    terminal_ansi_bright_green: Color::rgb(0x99_b7_82),
    terminal_ansi_dim_green: Color::rgb(0x53_6c_41),
    terminal_ansi_yellow: Color::rgb(0xf3_be_7c),
    terminal_ansi_dim_yellow: Color::rgb(0xa0_7d_51),
    terminal_ansi_bright_yellow: Color::rgb(0xf5_cb_96),
    terminal_ansi_blue: Color::rgb(0x6e_94_b2),
    terminal_ansi_bright_blue: Color::rgb(0x8b_a9_c1),
    terminal_ansi_dim_blue: Color::rgb(0x48_61_75),
    terminal_ansi_magenta: Color::rgb(0xbb_9d_bd),
    terminal_ansi_bright_magenta: Color::rgb(0xc9_b1_ca),
    terminal_ansi_dim_magenta: Color::rgb(0x7b_67_7c),
    terminal_ansi_cyan: Color::rgb(0xae_ae_d1),
    terminal_ansi_bright_cyan: Color::rgb(0xbe_be_da),
    terminal_ansi_dim_cyan: Color::rgb(0x72_72_89),
    terminal_ansi_white: Color::rgb(0xcd_cd_cd),
    terminal_ansi_bright_white: Color::rgb(0xd7_d7_d7),
    terminal_ansi_dim_white: Color::rgb(0x87_87_87),
    terminal_background: Color::rgb(0x14_14_15),
    terminal_foreground: Color::rgb(0xcd_cd_cd),
    terminal_bright_foreground: Color::rgb(0xd7_d7_d7),
    terminal_dim_foreground: Color::rgb(0x87_87_87),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_hex_preserves_transparency() {
        assert_eq!(Color::rgba(0x33_37_38_78).rgba_hex(), 0x33_37_38_78);
    }

    #[test]
    fn vague_pro_terminal_palette_has_normal_bright_and_dim_variants() {
        assert_eq!(
            (
                VAGUE_PRO.terminal_normal().len(),
                VAGUE_PRO.terminal_bright().len(),
                VAGUE_PRO.terminal_dim().len(),
            ),
            (8, 8, 8)
        );
    }

    #[test]
    fn vague_pro_interface_tokens_match_upstream() {
        assert_eq!(
            (
                VAGUE_PRO.text_muted.rgba_hex(),
                VAGUE_PRO.element_hover.rgba_hex(),
                VAGUE_PRO.ghost_element_background.rgba_hex(),
                VAGUE_PRO.scrollbar_thumb_hover_background.rgba_hex(),
                VAGUE_PRO.editor_invisible.rgba_hex(),
                VAGUE_PRO.panel_indent_guide_hover.rgba_hex(),
                VAGUE_PRO.search_match_background.rgba_hex(),
                VAGUE_PRO.search_current_match_background.rgba_hex(),
                VAGUE_PRO.tab_active_background.rgba_hex(),
            ),
            (
                0x87_87_87_ff,
                0x25_25_30_ff,
                0x00_00_00_00,
                0x60_60_79_78,
                0x60_60_79_ff,
                0x40_50_65_ff,
                0x6e_94_b2_66,
                0xe8_b5_89_66,
                0x25_25_30_ff,
            )
        );
    }
}
