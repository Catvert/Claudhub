//! L'exécuteur asynchrone partagé.
//!
//! Claudhub reste un programme à threads : les workers de `runtime::mod`
//! consomment des `Cmd` et lancent des sous-processus git, parce qu'un `fork`
//! bloque de toute façon et qu'il n'y a rien à entrelacer entre l'appel et le
//! résultat. Ce module n'y change rien — il **ajoute** un exécuteur tokio à
//! côté, pour les bibliothèques qui n'ont pas d'interface bloquante.
//!
//! La première est `sqlx`, dont tout le pilote est asynchrone. Ce qu'il
//! apporte et qu'un pilote bloquant ne pouvait pas donner :
//!
//! - **Un vrai délai.** `tokio::time::timeout` abandonne la requête en cours
//!   en laissant tomber son futur. Un pilote bloquant, lui, n'a rien à
//!   annuler : il faut le convaincre de s'arrêter tout seul — un rappel de
//!   progression pour SQLite, un délai de socket pour MySQL — et ce qui n'est
//!   pas prévu par le pilote ne s'interrompt pas du tout.
//! - **Une seule pile pour ce qui viendra.** Un client HTTP asynchrone pour
//!   Sentry, des sous-processus git lancés de front (`tokio::process`), un
//!   surveillant de fichiers : tout ce qui voudra de l'asynchrone trouvera
//!   l'exécuteur ici plutôt que d'en amener un second.
//!
//! **Le pont est `block_on`, et il est à un seul endroit** — le worker qui
//! traite la commande. C'est ce qui garde `runtime::handle` synchrone et pur :
//! il rend un `Vec<Evt>`, il ne connaît pas le canal, et il se teste. Un
//! worker qui attend un futur attend exactement comme il attendait `git`.
//!
//! **Jamais depuis le thread d'interface.** `block_on` y figerait la fenêtre,
//! ce qui est précisément ce que le protocole `Cmd`/`Evt` existe pour éviter ;
//! gpui a d'ailleurs son propre exécuteur pour ce dont la vue a besoin.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// Threads de l'exécuteur.
///
/// Deux, et non le nombre de cœurs que tokio prend par défaut : ce qui tourne
/// dessus attend une socket ou un fichier, il n'y a pas de calcul à répartir,
/// et une machine à seize cœurs n'a aucune raison de porter seize threads
/// endormis. C'est aussi ce qui borne la concurrence vers un serveur qu'on ne
/// veut pas inonder.
const WORKERS: usize = 2;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("claudhub-async")
            // `enable_all`: time (timeouts) and network (sockets). Without it a
            // `timeout` panics at run time announcing there is no timer — and
            // only once it is reached.
            .enable_all()
            .build()
            .expect("the system refuses to create the async executor")
    })
}

/// Awaits a future from a worker thread.
///
/// The executor starts on first call: a window that never opens a database
/// does not pay for its threads.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

/// A way to spawn a task on the shared executor.
///
/// Nothing uses it yet: it is the entry point for whatever wants to work side
/// by side — several queries, an HTTP client — without going back through a
/// worker that waits.
#[allow(dead_code)]
pub fn handle() -> tokio::runtime::Handle {
    runtime().handle().clone()
}
