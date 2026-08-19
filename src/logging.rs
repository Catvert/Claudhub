//! Journalisation.
//!
//! `PERCH_LOG` suit la syntaxe d'`env_logger` (`PERCH_LOG=debug`,
//! `PERCH_LOG=perch::git=trace`). Par défaut seuls les avertissements
//! remontent : les dépendances graphiques sont bavardes en info, et une
//! console noyée ne sert personne.
pub fn init() {
    env_logger::Builder::from_env(
        env_logger::Env::new()
            .filter_or("PERCH_LOG", "warn,perch=info")
            .write_style("PERCH_LOG_STYLE"),
    )
    .format_timestamp_millis()
    .init();
}
