//! Stats formatting, split by view (GL#440): `util` (shared helpers),
//! `cep` (CEP report), `dashboard` (gain hero), `views` (graph/daily/json).

mod cep;
mod dashboard;
mod util;
mod views;

pub use cep::format_cep_report;
pub use dashboard::{
    format_gain, format_gain_body, format_gain_footer, format_gain_hero, format_gain_hero_themed,
    format_gain_themed, format_gain_themed_at, gain_live,
};
pub(crate) use util::normalize_command;
pub use views::{format_gain_daily, format_gain_graph, format_gain_json};
