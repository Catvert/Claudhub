//! Journalisation.
//!
//! `CLAUDHUB_LOG` suit la syntaxe d'`env_logger` (`CLAUDHUB_LOG=debug`,
//! `CLAUDHUB_LOG=claudhub::git=trace`). Par défaut seuls les avertissements
//! remontent : les dépendances graphiques sont bavardes en info, et une
//! console noyée ne sert personne.
pub fn init() {
    env_logger::Builder::from_env(
        env_logger::Env::new()
            .filter_or("CLAUDHUB_LOG", "warn,claudhub=info")
            .write_style("CLAUDHUB_LOG_STYLE"),
    )
    .format_timestamp_millis()
    .init();
}
