//! Le binaire de l'interface : tout vit dans la bibliothèque `claudhub`.

fn main() {
    // Avant tout le reste, et avant qu'un thread existe : ce qui nous a lancés
    // est souvent un agent, et ses marqueurs de session feraient de chaque
    // agent que nous démarrons une sous-session du sien.
    claudhub::agent::disinherit_session();
    claudhub::logging::init();
    claudhub::ui::run();
}
