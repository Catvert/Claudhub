//! L'entité racine : l'état de la fenêtre et la pompe d'événements.
//!
//! Les sous-vues ne sont pas des entités séparées mais des `impl ClaudhubApp`
//! répartis par fichier (`sidebar`, `review`, `branches`). Tout ce qu'elles
//! affichent vient du même état, et le faire circuler entre entités coûterait
//! plus de code qu'il n'en économise. Les terminaux font exception : ils ont
//! leur propre cycle de vie et sont des `Entity<TerminalView>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Render, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    dock::{panel_handle, DockArea, DockLayout, DockPlacement, DockSkin},
    h_flex,
    input::{InputState, TextareaState},
    menu::{DropdownMenu, PopupMenuItem},
    select::{SearchableVec, SelectEvent, SelectState},
    separator::Separator as Divider,
    v_flex, ActiveTheme, Disableable, Root, Selectable, Sizable, StyledExt, WindowExt,
};

use crate::git::{Branch, Commit, DiffFile, DiffRange, GraphRow, LogRange, Status, Worktree};
use crate::runtime::watch::Watcher;
use crate::runtime::{self, Action, Cmd, Evt};
use crate::tr;
use std::sync::Arc;

use crate::ui::base_select::BaseChoice;
use crate::ui::diff_view::Rendered;
use crate::ui::icons::icon;
use crate::ui::panels::{
    BranchPanel, BranchesPanel, ChangesPanel, ConflictsPanel, DiffPanel, FilesPanel, HistoryPanel,
    NotesPanel, SentryPanel, SidebarPanel, TerminalPanel,
};
use crate::ui::settings::Settings;
use crate::ui::store::Store;
use crate::ui::terminal_view::TerminalGroup;

/// Hauteur d'origine du panneau des terminaux.
const TERMINAL_HEIGHT: gpui::Pixels = px(280.);

/// Version de la disposition enregistrée. À incrémenter quand les panneaux
/// changent de nom ou de nature, pour que gpui-component écarte une
/// disposition qu'il ne saurait plus reconstruire.
// 9 : le schéma de `DockAreaState` a changé avec la refonte du dock. Une
// disposition écrite par la version précédente se relirait de travers plutôt
// que de refuser franchement, ce qui donne une fenêtre pleine de cadres vides.
const LAYOUT_VERSION: usize = 9;

/// Les panneaux de la disposition par défaut.
///
/// `BasePanelView` et non `PanelView` : c'est le type que rend `panel_handle`,
/// et c'est **lui** qu'il faut. `Entity<P>` sait se convertir tout seul en
/// `Arc<dyn BasePanelView>` — et le dock le prend sans broncher, mais sans la
/// présentation qui va avec : ni onglet, ni titre, ni contenu. C'est la panne
/// silencieuse de cette refonte, et la seule chose que `panel_handle` empêche.
struct DefaultPanels {
    sidebar: Arc<dyn gpui_component::dock::BasePanelView>,
    branches: Arc<dyn gpui_component::dock::BasePanelView>,
    files: Arc<dyn gpui_component::dock::BasePanelView>,
    changes: Arc<dyn gpui_component::dock::BasePanelView>,
    branch: Arc<dyn gpui_component::dock::BasePanelView>,
    history: Arc<dyn gpui_component::dock::BasePanelView>,
    notes: Arc<dyn gpui_component::dock::BasePanelView>,
    sentry: Arc<dyn gpui_component::dock::BasePanelView>,
    conflicts: Arc<dyn gpui_component::dock::BasePanelView>,
    diff: Arc<dyn gpui_component::dock::BasePanelView>,
    terminal: Arc<dyn gpui_component::dock::BasePanelView>,
}

/// La disposition d'origine : les dépôts à gauche, la revue et le diff au
/// centre, les terminaux en dessous sur toute la largeur.
///
/// Le diff est dans le **centre** et non dans une zone d'accueil à droite :
/// les zones latérales occupent toute la hauteur, et le diff à droite couperait
/// les terminaux en deux au lieu de les laisser courir sous toute la revue.
fn install_default_layout(
    area: &mut DockArea,
    panels: DefaultPanels,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    // Les dépôts et les branches l'un au-dessus de l'autre, et non en onglets :
    // on choisit un worktree *puis* on regarde ses branches, et devoir passer
    // de l'un à l'autre pour cela est un aller-retour de trop. Un tiers pour
    // les branches, mesuré sur la fenêtre plutôt que fixé en pixels : la
    // proportion tient d'un écran à l'autre, là où un nombre de pixels
    // occuperait la moitié d'une petite fenêtre.
    //
    // Les fichiers du projet sont l'onglet voisin des dépôts, et non celui des
    // branches : ce sont deux façons de désigner ce qu'on veut ouvrir — un
    // worktree, un fichier dedans —, et l'arbre d'un projet est ce qui a le
    // plus besoin des deux tiers du haut. Les branches, elles, sont une liste
    // courte qu'on filtre.
    let height = window.viewport_size().height.max(px(600.));
    let third = height / 3.;
    let left = DockLayout::v_split()
        .child(
            DockLayout::tabs()
                .panel_view(panels.sidebar, cx)
                .panel_view(panels.files, cx),
            Some(height - third),
        )
        .child(
            DockLayout::tabs().panel_view(panels.branches, cx),
            Some(third),
        );

    let center = DockLayout::v_split()
        .child(
            DockLayout::h_split()
                // Les façons de choisir quoi relire : ce qui reste à faire et
                // ce qu'on a eu à dire, ce qui change maintenant, ce que la
                // branche a écrit, ce qui est déjà committé. Des onglets et non
                // des panneaux côte à côte — ils répondent à la même question,
                // et se glissent ailleurs d'un geste si l'on préfère les voir
                // ensemble.
                .child(
                    DockLayout::tabs()
                        // Les notes en premier : elles disent où l'on en est,
                        // là où les suivantes disent ce qu'il y a à lire. C'est
                        // par là qu'on reprend un worktree quitté hier.
                        .panel_view(panels.notes, cx)
                        .panel_view(panels.changes, cx)
                        .panel_view(panels.branch, cx)
                        .panel_view(panels.history, cx)
                        // Les issues sont un point de départ comme un autre,
                        // souvent meilleur qu'une intention : elles se lisent
                        // où l'on choisit quoi relire.
                        .panel_view(panels.sentry, cx)
                        // Masqué tant qu'il n'y a rien à résoudre : un onglet
                        // permanent décalerait les autres pour servir une fois
                        // sur cent.
                        .panel_view(panels.conflicts, cx),
                    Some(px(420.)),
                )
                .child(DockLayout::tabs().panel_view(panels.diff, cx), None),
            Some(height - TERMINAL_HEIGHT),
        )
        // Les terminaux vivent dans le centre et non dans une zone d'accueil :
        // le dernier panneau d'une zone ne se déplace pas, et une zone qui n'en
        // contient qu'un est donc figée. Ici la pile en compte deux — il se
        // glisse.
        .child(
            DockLayout::tabs().panel_view(panels.terminal, cx),
            Some(TERMINAL_HEIGHT),
        );

    area.set_center(center, window, cx);
    area.set_dock(DockPlacement::Left, left, window, cx);
    area.set_dock_size(DockPlacement::Left, px(280.), window, cx);
}

fn load_layout() -> Option<gpui_component::dock::DockAreaState> {
    let path = crate::ui::settings::layout_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(state) => Some(state),
        Err(e) => {
            // Une disposition illisible n'est pas une raison de ne pas
            // démarrer : on repart de celle par défaut.
            log::warn!("disposition illisible : {e}");
            None
        }
    }
}

fn save_layout(state: &gpui_component::dock::DockAreaState) {
    let Some(path) = crate::ui::settings::layout_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("écriture de la disposition : {e}");
            }
        }
        Err(e) => log::warn!("sérialisation de la disposition : {e}"),
    }
}

/// Un dépôt ouvert dans la barre latérale.
pub struct RepoState {
    pub main: PathBuf,
    pub name: String,
    pub worktrees: Vec<Worktree>,
    pub branches: Vec<Branch>,
    /// Branche d'intégration, telle que git la déclare. Elle n'est connue
    /// qu'après la réponse du worker : jusque-là, la revue de branche n'a pas
    /// de base et son onglet reste inactif — proposer un `main` supposé
    /// produirait un « unknown revision » sur tout dépôt qui ne s'appelle pas
    /// ainsi.
    pub default_base: Option<String>,
    pub collapsed: bool,
}

/// Ce que la revue montre pour un worktree donné.
///
/// Un état par worktree, et non un seul état global : passer d'un worktree à
/// l'autre pour comparer est le geste central de l'outil, et il ne doit pas
/// coûter la perte du fichier qu'on était en train de lire.
pub struct ReviewState {
    /// Domaine du diff affiché à droite. Ce n'est plus le domaine « courant »
    /// de la revue — chaque panneau a le sien — mais celui du fichier sur
    /// lequel on a cliqué en dernier, quel que soit le panneau d'où il vient.
    pub range: DiffRange,
    pub status: Status,
    /// Les fichiers touchés, **par domaine**. Deux panneaux affichent deux
    /// listes en même temps : une seule liste ferait clignoter l'une chaque
    /// fois que l'autre se recharge.
    pub files: HashMap<DiffRange, Vec<DiffFile>>,
    /// Domaines dont la liste est demandée et pas encore revenue. C'est le
    /// panneau qui demande, et il demande au rendu : sans ce garde, chaque
    /// frame relancerait la commande.
    pub pending_files: std::collections::HashSet<DiffRange>,
    pub selected: Option<PathBuf>,
    /// Le diff affiché, avec tout ce qui s'en déduit. Un `Rc` parce que le
    /// rendu doit le capturer dans la fermeture de la liste virtualisée, et
    /// qu'en copier plusieurs milliers de lignes par frame reviendrait à
    /// annuler le bénéfice de la virtualisation.
    pub diff: Option<std::rc::Rc<Rendered>>,
    /// Lignes sélectionnées dans le diff : l'ancre et la tête, en indices de
    /// la liste mise à plat. Deux indices et non une plage triée, parce que
    /// c'est le sens du geste qui décide de laquelle bouge à la prochaine
    /// extension.
    pub diff_selection: Option<(usize, usize)>,
    /// Dossiers repliés dans la liste des fichiers.
    ///
    /// Un seul jeu pour les deux groupes : replier `src/ui` veut dire replier
    /// ce dossier, pas « ce dossier parmi les fichiers suivis ».
    pub collapsed: std::collections::HashSet<PathBuf>,
    /// Base de comparaison de la revue de branche, devinée à l'ouverture.
    pub base: Option<String>,
    /// L'historique et son graphe, chargés à la demande — ouvrir un worktree
    /// ne doit pas payer un `git log` que personne ne regardera.
    pub history: Option<std::rc::Rc<History>>,
    pub history_range: LogRange,
    /// Une lecture de l'historique est partie et n'est pas revenue.
    ///
    /// Sans ce garde, le panneau redemanderait un `git log` à chaque frame
    /// pendant tout le temps de la commande : c'est lui qui demande, et il
    /// demande au rendu.
    pub history_pending: bool,
    /// Commit sélectionné dans l'historique, dont le diff est affiché.
    pub commit: Option<String>,
    /// Les notes de relecture prises sur ce worktree, tous fichiers confondus.
    ///
    /// Elles vivent ici et non dans le diff : une note survit au rechargement
    /// du fichier, au changement de domaine et à la fermeture de Claudhub, alors
    /// qu'un `Rendered` ne survit pas à la prochaine écriture de fichier.
    pub notes: Vec<crate::ui::notes::Note>,
    /// Prochain identifiant. Il ne se déduit pas de `notes` : une note
    /// supprimée libérerait son numéro, et deux notes le porteraient.
    pub next_note: u64,
    /// Les fichiers qu'on a marqués relus, avec le volume qu'ils avaient
    /// alors. Comme les notes, ils vivent dans le dossier de notes et non dans
    /// le magasin d'état : c'est la même relecture, et elle se lit d'un coup.
    pub reviewed: Vec<crate::ui::vault::Reviewed>,
    /// La note libre du worktree : `NOTES.md`, tel qu'il est sur le disque.
    ///
    /// Une chaîne et non un `Option` : vide veut dire « pas de fichier », et
    /// les deux ne s'affichent pas différemment — la zone de saisie est là de
    /// toute façon, c'est justement ce qui la rend disponible sans geste.
    pub journal: String,
    /// La liste de tâches du worktree, si le coffre en porte une.
    ///
    /// `None` veut dire « pas de `TODO.md` », pas « aucune tâche » : les deux
    /// ne s'affichent pas pareil, et Claudhub ne pose pas ce fichier tout seul
    /// — c'est celui de l'agent, on ne sème pas une liste vide dans le coffre
    /// de tout worktree qu'on ouvre.
    pub todo: Option<crate::ui::vault::Todo>,
    /// Le dossier de notes a répondu. Tant qu'il n'a pas répondu, il n'y a
    /// rien à écrire : le faire effacerait ce qu'on n'a pas encore lu.
    pub notes_loaded: bool,
    /// Ce worktree a déjà un dossier de notes.
    ///
    /// Sans ce drapeau, ouvrir un worktree suffirait à créer son dossier et un
    /// index vide : un coffre finirait avec une arborescence de dossiers vides
    /// pour des worktrees que personne n'a annotés. Il reste vrai une fois la
    /// dernière note supprimée, sans quoi l'effacement ne partirait jamais.
    pub notes_on_disk: bool,
    /// Les lignes annotées du diff affiché, une case par entrée de liste.
    ///
    /// Calculé **en amont**, à l'arrivée du diff et à chaque modification des
    /// notes, jamais dans la fermeture de `uniform_list` : celle-ci tourne
    /// pour chaque ligne visible à chaque frame, animation de molette
    /// comprise.
    pub note_marks: std::rc::Rc<crate::ui::notes::Marks>,
    /// Notes que le diff affiché ne sait plus placer.
    ///
    /// Elles restent dans la liste, marquées comme telles : une note perdue en
    /// silence est pire que pas de note du tout. L'ensemble ne vaut que pour
    /// le fichier ouvert — c'est le seul dont on ait le diff sous la main.
    pub drifted: std::collections::HashSet<u64>,
    /// Où poser la sélection quand le diff demandé arrivera.
    ///
    /// Une flèche qui déborde sur le fichier voisin ne peut pas le placer
    /// elle-même : le diff n'arrive qu'après la commande git. Le geste est
    /// donc noté, et consommé à l'arrivée — seulement pour la navigation au
    /// clavier, un clic devant ouvrir un fichier sans rien y sélectionner.
    pub pending_jump: Option<Jump>,
    /// Note dont il faudra sélectionner les lignes à l'arrivée du diff.
    ///
    /// Même raison que `pending_jump` : cliquer une note du panneau ouvre un
    /// fichier, et son diff n'arrive qu'après la commande git.
    pub pending_note: Option<u64>,
}

/// De quel bout un fichier ouvert au clavier commence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Descendre : on entre par la première modification.
    First,
    /// Remonter : on entre par la dernière, là où la lecture s'arrête.
    Last,
}

/// L'historique tel que la vue l'affiche.
pub struct History {
    pub commits: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    /// Nombre de colonnes du graphe, pour dimensionner sa gouttière.
    pub width: usize,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            range: DiffRange::Working,
            status: Status::default(),
            files: HashMap::new(),
            pending_files: std::collections::HashSet::new(),
            selected: None,
            diff: None,
            diff_selection: None,
            collapsed: std::collections::HashSet::new(),
            base: None,
            history: None,
            history_range: LogRange::All,
            history_pending: false,
            commit: None,
            notes: Vec::new(),
            next_note: 1,
            reviewed: Vec::new(),
            journal: String::new(),
            todo: None,
            notes_loaded: false,
            notes_on_disk: false,
            note_marks: std::rc::Rc::new(crate::ui::notes::Marks::default()),
            drifted: std::collections::HashSet::new(),
            pending_jump: None,
            pending_note: None,
        }
    }
}

/// Ce qu'on sait des agents d'un worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentState {
    pub count: usize,
    /// Les agents trouvés, par nom de programme et sans doublon.
    ///
    /// La barre latérale dit *quel* agent tourne : à deux profils près, « un
    /// agent travaille ici » ne dit pas lequel, et c'est justement ce qu'on
    /// regarde en parcourant la liste.
    pub programs: Vec<String>,
    /// Vrai quand au moins un agent a consommé du processeur depuis le relevé
    /// précédent.
    ///
    /// C'est une approximation assumée : rien dans un processus ne dit « je
    /// réfléchis » ou « j'attends une réponse ». Un agent qui travaille redessine
    /// son affichage plusieurs fois par seconde et se voit ; un agent qui attend
    /// une réponse de l'utilisateur ne coûte rien.
    pub working: bool,
}

/// Consommation en dessous de laquelle un agent est réputé en attente.
///
/// Un tic vaut dix millisecondes de processeur. Trois tics sur un intervalle
/// de trois secondes, c'est un pour cent d'un cœur : au-dessus, il se passe
/// quelque chose ; en dessous, c'est le clignotement d'un curseur.
const AGENT_BUSY_TICKS: u64 = 3;

/// Période du relevé des agents. Une lecture de `/proc`, sans processus
/// lancé : assez court pour qu'un agent qui se met au travail se voie.
const AGENT_PERIOD: std::time::Duration = std::time::Duration::from_secs(2);

/// Un résumé sur cinq relevés.
///
/// Le résumé coûte **deux commandes git par worktree** ; à ce prix, le faire
/// aussi souvent que le relevé des agents ferait tourner un worker en
/// permanence sur une dizaine de worktrees. Un compte de lignes qui a dix
/// secondes de retard ne trompe personne.
const SUMMARY_EVERY: u32 = 5;

/// Le résultat de la dernière action, affiché dans la barre d'état.
pub struct Toast {
    pub text: SharedString,
    pub error: bool,
}

pub struct ClaudhubApp {
    pub(super) git: runtime::Handle,
    pub(super) repos: Vec<RepoState>,
    /// Worktree sélectionné : la clé de presque tout le reste.
    pub(super) active: Option<PathBuf>,
    pub(super) review: HashMap<PathBuf, ReviewState>,
    pub(super) terminals: HashMap<PathBuf, Entity<TerminalGroup>>,
    pub(super) commit_input: Entity<TextareaState>,
    /// Sélecteur de la base de comparaison. Il est searchable : un dépôt
    /// vivant a des dizaines de branches, et faire défiler une liste de
    /// soixante-dix entrées pour en trouver une dont on connaît le nom est
    /// exactement ce qu'un champ de recherche évite.
    pub(super) base_select: Entity<SelectState<SearchableVec<BaseChoice>>>,
    /// Champ de saisie d'une note. Créé **une fois** : recréé dans un `render`
    /// ou à l'ouverture du dialogue, il perdrait curseur, sélection et texte
    /// dès la première frappe.
    pub(super) note_input: Entity<TextareaState>,
    /// Le prompt qui part à l'agent, avant qu'il parte.
    pub(super) prompt_input: Entity<TextareaState>,
    /// La saisie d'une tâche à ajouter à `TODO.md`, en bas de la liste.
    pub(super) task_input: Entity<InputState>,
    /// La saisie d'une tâche qu'on retouche, à sa place dans la liste.
    ///
    /// Une seconde zone et non celle du bas : ce qu'on est en train de taper
    /// pour une nouvelle tâche ne doit pas disparaître parce qu'on corrige une
    /// faute deux lignes plus haut.
    pub(super) task_edit_input: Entity<InputState>,
    /// La ligne de la tâche en cours de retouche, s'il y en a une.
    pub(super) task_editing: Option<usize>,
    /// La zone de saisie de la note libre du worktree.
    ///
    /// Une seule pour tous les worktrees, dont le contenu suit celui qui est
    /// affiché : une par worktree ouvert garderait autant d'états d'édition
    /// vivants, et il n'y en a jamais qu'un sous les yeux.
    pub(super) journal_input: Entity<TextareaState>,
    /// Une écriture de la note libre est déjà programmée.
    pub(super) journal_save: bool,
    /// La note en cours de rédaction : son ancrage, arrêté au moment du geste.
    ///
    /// Il est arrêté là et non à la validation parce que le diff peut changer
    /// pendant qu'on écrit — un agent travaille pendant qu'on le relit — et
    /// que la note doit porter sur ce qu'on avait sous les yeux.
    pub(super) note_draft: Option<crate::ui::notes_view::NoteDraft>,
    /// Les sections repliées du panneau « Notes », par clé.
    ///
    /// En mémoire et non dans le magasin : c'est une posture de lecture, qui
    /// change plusieurs fois pendant une relecture, pas une préférence qu'on
    /// s'attend à retrouver le lendemain.
    pub(super) notes_collapsed: std::collections::HashSet<&'static str>,
    /// Le panneau des notes ne montre-t-il que les non traitées.
    pub(super) notes_only_open: bool,
    /// Le `wt.toml` de chaque dépôt ouvert, lu une fois. `None` : il n'en a
    /// pas, et les gestes du projet disparaissent simplement du menu.
    pub(super) wt_projects: HashMap<PathBuf, Option<crate::wt::Snapshot>>,
    /// Ce que `wt` sait de chaque worktree : démarré ou non, ses adresses.
    pub(super) wt_states: HashMap<PathBuf, crate::runtime::protocol::WtWorktree>,
    /// La création guidée en cours.
    pub(super) creation: Option<crate::ui::worktree_ops::Creation>,
    /// Le worktree dont l'intégration est partie, et sa branche : c'est à
    /// l'arrivée du succès qu'on propose de faire le ménage.
    pub(super) integrated: Option<(PathBuf, String)>,
    /// Les fichiers de chaque worktree et leur arborescence.
    pub(super) explorers: HashMap<PathBuf, crate::ui::explorer::Explorer>,
    /// Le fichier ouvert dans l'éditeur intégré, s'il y en a un.
    pub(super) editing: Option<crate::ui::explorer::Editing>,
    pub(super) files_scroll: gpui::UniformListScrollHandle,
    /// Le focus de l'arbre de l'explorateur, qui lui donne ses flèches.
    ///
    /// Un handle à lui et non celui de la vue racine : c'est ce qui distingue
    /// « les flèches parcourent l'arborescence » de « les flèches parcourent
    /// le diff », et le prédicat des liaisons se lit sur le contexte du nœud
    /// focalisé.
    pub(super) explorer_focus: FocusHandle,
    /// Les issues Sentry du dépôt courant, et celle qu'on regarde.
    pub(super) sentry: crate::ui::sentry_view::SentryState,
    /// Un worktree qu'on attend, et le prompt à y livrer une fois créé.
    ///
    /// La création de `wt` lance des hooks qui durent des minutes ; rien
    /// d'autre que l'arrivée de la liste des worktrees ne dit qu'elle a fini.
    pub(super) awaiting_agent: Option<(PathBuf, String)>,
    pub(super) toast: Option<Toast>,
    /// Worktrees dont une lecture de statut est déjà partie.
    ///
    /// Le surveillant de fichiers peut produire plusieurs vagues avant qu'une
    /// réponse revienne ; sans ce garde-fou, une compilation qui touche mille
    /// fichiers empile mille `git status` identiques, et tout ce qui suit —
    /// diffs compris — attend derrière eux.
    pending_status: std::collections::HashSet<PathBuf>,
    /// La disposition. Elle appartient à gpui-component : c'est lui qui gère
    /// le glissement d'un panneau d'une zone à l'autre, les onglets et les
    /// zones d'accueil.
    pub(super) dock: Entity<DockArea>,
    /// La peau du dock, gardée en vie : c'est elle qui dessine les onglets, et
    /// c'est par elle que passent les réglages de présentation.
    #[allow(dead_code)]
    dock_skin: std::rc::Rc<DockSkin>,
    /// Vrai quand une écriture différée de la disposition est déjà programmée.
    layout_save_scheduled: bool,
    /// Les vues que l'utilisateur a masquées, par nom de panneau.
    ///
    /// Un ensemble et non un drapeau par panneau : c'est `Panel::visible` qui
    /// fait disparaître une vue — une zone d'accueil repliable interdirait de
    /// déplacer le dernier panneau qui y reste —, et le mécanisme est le même
    /// pour les terminaux que pour la revue.
    ///
    /// Ici et non dans les réglages seuls : les panneaux l'observent, et
    /// `Settings::update_global` ne notifie personne. Les réglages en gardent
    /// la copie qui survit à la fermeture.
    pub(super) hidden_panels: std::collections::HashSet<String>,

    /// Ce que chaque worktree a en chantier, y compris ceux qu'on n'a pas
    /// ouverts : c'est la question qu'on se pose en parcourant la liste.
    pub(super) summaries: HashMap<PathBuf, crate::git::Summary>,
    /// Les agents trouvés, par worktree, et s'ils travaillent.
    pub(super) agents: HashMap<PathBuf, AgentState>,
    /// Temps processeur du relevé précédent, par processus. C'est sa variation
    /// qui distingue un agent au travail d'un agent qui attend.
    agent_cpu: HashMap<u32, u64>,
    /// Vrai entre l'enfoncement et le relâchement du bouton dans la vue de
    /// diff : c'est ce qui distingue un glissement de sélection d'un simple
    /// survol, et ce qui empêche un glissement commencé ailleurs — dans la
    /// barre latérale, sur une poignée de redimensionnement — d'étendre la
    /// sélection en passant au-dessus du code.
    pub(super) diff_dragging: bool,

    /// Surveillance du worktree affiché. `None` si le système refuse de nous
    /// donner un observateur (limite d'inotify atteinte, par exemple) : Claudhub
    /// marche encore, il faut seulement actualiser à la main.
    watcher: Option<Watcher>,

    /// Défilement de la liste virtualisée du diff. Il vit sur la vue et n'est
    /// jamais reconstruit : le recréer par frame remettrait le diff en haut à
    /// chaque image.
    pub(super) diff_scroll: gpui::UniformListScrollHandle,
    /// La largeur mesurée de la vue de diff à la frame précédente.
    ///
    /// Une vue qu'on vient d'ouvrir n'a pas de bornes : elles ne valent
    /// quelque chose qu'une fois la première mise en page faite. Les prendre
    /// pour la largeur réelle replie le fichier à huit colonnes — et comme
    /// rien ne redessine tant qu'il ne se passe rien, l'affichage reste faux
    /// jusqu'au prochain événement, soit le balayage de fond, deux secondes
    /// plus tard. On garde donc la dernière largeur connue, qui vaut pour tous
    /// les diffs suivants.
    pub(super) diff_width: gpui::Pixels,
    /// Frames demandées en attendant cette première mesure.
    ///
    /// Bornées : un panneau qu'on aurait rétréci à zéro ne serait jamais
    /// mesuré, et redemander une frame à chaque frame ferait tourner
    /// l'interface à plein régime pour une vue que personne ne voit.
    pub(super) diff_measures: u8,
    /// La poignée de la vue en deux colonnes repliée.
    ///
    /// Une seconde poignée et non la même : les entrées n'y ont plus la même
    /// hauteur, c'est `v_virtual_list` qui les peint, et il ne sait défiler
    /// qu'avec la sienne. Les deux ne sont jamais affichées en même temps.
    pub(super) diff_wrap_scroll: gpui_component::VirtualListScrollHandle,
    pub(super) history_scroll: gpui::UniformListScrollHandle,
    pub(super) branch_scroll: gpui::UniformListScrollHandle,
    /// Défilement des listes de fichiers, **une par domaine** : « Revue » et
    /// « Modifications » sont affichés en même temps, et une seule poignée les
    /// ferait défiler ensemble.
    file_scroll: HashMap<DiffRange, gpui::UniformListScrollHandle>,
    /// La recherche de chaque panneau, créée à sa première ouverture.
    pub(super) finders: HashMap<crate::ui::find::Pane, crate::ui::find::Finder>,
    /// Le panneau où le dernier clic a eu lieu : c'est lui que `Ctrl+F` vise.
    ///
    /// Le clic et non le focus. Le dock de gpui-component pose le focus sur
    /// l'onglet actif de chaque zone — il y en a trois affichées en même
    /// temps — et rien ne dit laquelle l'utilisateur regarde ; le dernier
    /// bouton enfoncé, si.
    pub(super) pane: crate::ui::find::Pane,
    /// Les occurrences trouvées dans le diff affiché.
    pub(super) diff_search: crate::ui::find::DiffSearch,
    /// Poignées de défilement des panneaux qui ne sont **pas** virtualisés —
    /// les notes, les conflits, Sentry, la barre latérale. Une table plutôt
    /// qu'un champ par panneau : ce sont toutes la même chose, et elles ne
    /// servent qu'à donner une position à la barre de défilement. Créées ici
    /// et non au rendu, sans quoi la liste remonterait en haut à chaque frame.
    scrolls: HashMap<&'static str, gpui::ScrollHandle>,
    /// Le lissage de la molette, par panneau, créé à sa première utilisation.
    /// La clé est celle de la barre de défilement — voir `ui::scroll`.
    pub(super) motions: HashMap<gpui::SharedString, crate::ui::motion::ScrollMotion>,
    /// Filtre du panneau des branches. Une entité créée une fois : recréée par
    /// frame, elle perdrait le curseur et le texte dès la première frappe.
    pub(super) branch_filter: Entity<InputState>,
    /// Partage entre le graphe et la liste des fichiers du commit choisi.
    pub(super) history_split: Entity<gpui_component::resizable::ResizableState>,
    focus: FocusHandle,
}

/// Une ligne du menu des vues : la coche, le nom, et le geste qui bascule.
///
/// Un `PopupMenuItem::element` et non une entrée ordinaire, pour deux raisons
/// qui tiennent toutes deux à la même chose — **on en bascule plusieurs à la
/// suite** :
///
/// - `PopupMenu::confirm` **referme le menu** après avoir appelé le
///   gestionnaire d'une entrée, sans qu'on puisse s'y opposer. La ligne
///   consomme donc le clic elle-même (`stop_propagation`) : l'entrée qui la
///   porte ne le voit jamais, et rien ne se referme.
/// - Un `checked` est figé à la construction du menu, qui n'a lieu qu'une
///   fois. La coche est donc peinte par la ligne, qui relit l'état à chaque
///   frame.
fn view_toggle(app: Entity<ClaudhubApp>, name: &'static str, title: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let visible = app.read(cx).panel_visible(name);
        let app = app.clone();
        h_flex()
            .id(name)
            .w_full()
            .gap_2()
            .items_center()
            // La colonne de la coche est réservée en permanence : sans elle,
            // les noms danseraient d'un cran à chaque bascule.
            .child(
                div()
                    .w(px(14.))
                    .when(visible, |this| this.child(icon("check").xsmall())),
            )
            .child(tr!(title))
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                app.update(cx, |this, cx| this.toggle_panel(name, cx));
            })
    })
}

impl ClaudhubApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (git, events) = runtime::spawn();

        let commit_input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("commit-placeholder")));

        let branch_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("branch-filter-placeholder")));

        // `auto_grow` plutôt qu'une hauteur fixe : une remarque de relecture
        // fait deux lignes ou dix, et une zone figée oblige à faire défiler ce
        // qu'on est en train d'écrire.
        let note_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder(tr!("note-placeholder"))
        });

        // Plus haut que celui d'une note : ce qu'on relit ici est un message
        // entier, avec le code cité, et huit lignes de contexte sont le
        // minimum pour juger de ce qui part.
        let prompt_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 20));

        let task_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("todo-add-placeholder")));
        let task_edit_input = cx.new(|cx| InputState::new(window, cx));

        // La note libre : elle grandit avec ce qu'on y écrit, dans les limites
        // que la section peut donner sans repousser le reste hors de vue.
        let journal_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 14)
                .placeholder(tr!("journal-placeholder"))
        });

        let base_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<BaseChoice>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        // Souscrit une fois, dans le constructeur : une souscription posée
        // pendant un rendu s'accumulerait à chaque frame.
        cx.subscribe(&base_select, |this, _, event, cx| {
            let SelectEvent::Confirm(Some(base)) = event else {
                return;
            };
            this.set_base(base.to_string(), cx);
        })
        .detach();

        // Le dock et ses panneaux. Les panneaux ne portent aucun état : ils
        // délèguent à cette entité, dont ils ne gardent qu'une référence
        // faible pour ne pas former de cycle.
        let this = cx.entity();
        // **Par `DockSkin` et non par `DockArea::new`.** Depuis que le moteur
        // de disposition vit dans `gpui-base`, une aire construite sans peau
        // dock, glisse et persiste très bien — mais ne dessine **aucun
        // chrome** : ni barre d'onglets, ni titre, ni cadre. Les panneaux
        // s'empilent alors nus, ce qui se lit comme une fenêtre cassée sans
        // qu'une seule erreur soit signalée.
        let (dock, dock_skin) = DockSkin::dock_area("claudhub", Some(LAYOUT_VERSION), window, cx);
        // `Segmented` : la pastille arrondie dans un rail, à la place du
        // rectangle bordé dont le rayon est un zéro codé en dur. C'est notre
        // commit sur le fork qui expose ce réglage.
        dock_skin.set_tab_variant(gpui_component::tab::TabVariant::Segmented, cx);
        // Barre d'onglets partout, y compris sur les groupes d'un seul
        // panneau : le défaut (`Auto`) rend alors un titre plat, et
        // « Branches » ou « Terminaux » n'avaient pas le même bandeau que
        // leurs voisins — deux chromes pour une même fenêtre.
        dock_skin.set_panel_style(gpui_component::dock::PanelStyle::TabBar, cx);

        crate::ui::panels::register(&this, cx);

        // Une disposition enregistrée reprend la main sur celle par défaut.
        // Elle est écartée si sa version diffère : les panneaux ont pu changer
        // de nom, et reconstruire à partir de noms inconnus donnerait une
        // fenêtre pleine de cadres vides.
        let mut app_needs_layout_save = false;
        let restored = load_layout()
            .filter(|state| state.version == Some(LAYOUT_VERSION))
            .and_then(|state| {
                dock.update(cx, |area, cx| area.load(state, window, cx))
                    .ok()
            })
            .is_some();
        let sidebar = cx.new(|cx| SidebarPanel::new(&this, cx));
        let branches = cx.new(|cx| BranchesPanel::new(&this, cx));
        let files = cx.new(|cx| FilesPanel::new(&this, cx));
        let changes = cx.new(|cx| ChangesPanel::new(&this, cx));
        let branch = cx.new(|cx| BranchPanel::new(&this, cx));
        let history = cx.new(|cx| HistoryPanel::new(&this, cx));
        let notes = cx.new(|cx| NotesPanel::new(&this, cx));
        let sentry = cx.new(|cx| SentryPanel::new(&this, cx));
        let conflicts = cx.new(|cx| ConflictsPanel::new(&this, cx));
        let diff = cx.new(|cx| DiffPanel::new(&this, cx));
        let terminal = cx.new(|cx| TerminalPanel::new(&this, cx));

        if !restored {
            let panels = DefaultPanels {
                sidebar: panel_handle(sidebar),
                branches: panel_handle(branches),
                files: panel_handle(files),
                changes: panel_handle(changes),
                branch: panel_handle(branch),
                history: panel_handle(history),
                notes: panel_handle(notes),
                sentry: panel_handle(sentry),
                conflicts: panel_handle(conflicts),
                diff: panel_handle(diff),
                terminal: panel_handle(terminal),
            };
            dock.update(cx, |area, cx| {
                install_default_layout(area, panels, window, cx);
            });
        }

        // La disposition d'origine est écrite tout de suite : sans cela, le
        // fichier garde celle d'une version antérieure jusqu'au premier
        // déplacement, et c'est elle qu'on relirait au prochain démarrage.
        if !restored {
            app_needs_layout_save = true;
        }

        // Le dock notifie à chaque déplacement, redimensionnement ou
        // changement d'onglet : c'est le signal d'enregistrement, différé pour
        // qu'un glissement n'écrive pas un fichier par pixel.
        cx.observe(&dock, |this, _, cx| this.schedule_layout_save(cx))
            .detach();

        let mut app = Self {
            git,
            repos: Vec::new(),
            active: None,
            review: HashMap::new(),
            terminals: HashMap::new(),
            commit_input,
            base_select,
            note_input,
            prompt_input,
            task_input,
            task_edit_input,
            task_editing: None,
            journal_input,
            journal_save: false,
            notes_collapsed: std::collections::HashSet::new(),
            note_draft: None,
            notes_only_open: false,
            wt_projects: HashMap::new(),
            wt_states: HashMap::new(),
            creation: None,
            integrated: None,
            explorers: HashMap::new(),
            editing: None,
            files_scroll: gpui::UniformListScrollHandle::new(),
            sentry: Default::default(),
            awaiting_agent: None,
            toast: None,
            pending_status: std::collections::HashSet::new(),
            dock,
            dock_skin,
            layout_save_scheduled: false,
            hidden_panels: Settings::global(cx).hidden_panels.iter().cloned().collect(),
            summaries: HashMap::new(),
            agents: HashMap::new(),
            agent_cpu: HashMap::new(),
            diff_dragging: false,
            watcher: None,
            diff_scroll: gpui::UniformListScrollHandle::new(),
            diff_width: gpui::px(0.),
            diff_measures: 0,
            diff_wrap_scroll: gpui_component::VirtualListScrollHandle::new(),
            history_scroll: gpui::UniformListScrollHandle::new(),
            branch_scroll: gpui::UniformListScrollHandle::new(),
            file_scroll: HashMap::new(),
            scrolls: HashMap::new(),
            motions: HashMap::new(),
            explorer_focus: cx.focus_handle(),
            finders: HashMap::new(),
            // Le diff par défaut : c'est le panneau qu'on regarde en arrivant,
            // et le seul qui soit toujours là.
            pane: crate::ui::find::Pane::Diff,
            diff_search: crate::ui::find::DiffSearch::default(),
            branch_filter,
            history_split: cx.new(|_| gpui_component::resizable::ResizableState::default()),
            focus: cx.focus_handle(),
        };

        if app_needs_layout_save {
            app.schedule_layout_save(cx);
        }
        app.pump_events(events, window, cx);
        app.start_scanning(cx);
        app.start_watching(window, cx);
        app.watch_vault_inputs(window, cx);

        // Les dépôts de la session précédente, puis le répertoire courant s'il
        // en est un — c'est ce qu'attend quelqu'un qui lance `claudhub` depuis son
        // projet.
        let remembered = Settings::global(cx).repositories.clone();
        for path in remembered {
            app.git.send(Cmd::OpenRepo(path));
        }
        if let Ok(cwd) = std::env::current_dir() {
            if crate::git::repo::is_repo(&cwd) {
                app.git.send(Cmd::OpenRepo(cwd));
            }
        }
        app
    }

    /// Interroge périodiquement l'état de tous les worktrees ouverts.
    ///
    /// Le surveillant de fichiers ne couvre que le worktree affiché ; les
    /// autres — ceux où un agent travaille pendant qu'on relit ailleurs — ne
    /// signalent rien. Un balayage régulier est le seul moyen de les voir
    /// bouger, et c'est justement là que se passe ce qu'on veut voir.
    fn start_scanning(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut tick: u32 = 0;
            loop {
                let alive = this
                    .update(cx, |this, cx| {
                        this.scan_now(tick.is_multiple_of(SUMMARY_EVERY), cx);
                    })
                    .is_ok();
                if !alive {
                    return;
                }
                tick = tick.wrapping_add(1);
                cx.background_executor().timer(AGENT_PERIOD).await;
            }
        })
        .detach();
    }

    fn scan_now(&mut self, with_summaries: bool, cx: &mut Context<Self>) {
        let worktrees: Vec<PathBuf> = self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter().map(|w| w.path.clone()))
            .collect();
        if worktrees.is_empty() {
            return;
        }
        if with_summaries {
            self.git.send(Cmd::LoadSummaries {
                worktrees: worktrees.clone(),
            });
            // Le relevé de `wt` suit celui des résumés : ce sont des commandes
            // shell déclarées par le projet, une par worktree, et il n'y a
            // aucune raison de les lancer plus souvent qu'un `git status`.
            self.scan_wt();
        }
        let programs = Settings::global(cx).terminal.agent_programs();
        self.git.send(Cmd::ScanAgents {
            worktrees,
            programs,
        });
    }

    /// Enregistre la disposition, une fois le calme revenu.
    ///
    /// L'état est relu au moment d'écrire et non à l'appel : l'ouverture d'une
    /// zone d'accueil est différée d'une frame, et le capturer tout de suite
    /// enregistrerait l'état d'avant le geste.
    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        if self.layout_save_scheduled {
            return;
        }
        self.layout_save_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(700))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.layout_save_scheduled = false;
                save_layout(&this.dock.read(cx).dump(cx));
            });
        })
        .detach();
    }

    /// Draine les événements des workers par lots.
    ///
    /// Par lots parce qu'un `update_in` par événement force un cycle d'effets
    /// gpui à chaque fois : une ouverture de dépôt qui en produit une dizaine
    /// coûterait dix rendus au lieu d'un.
    fn pump_events(
        &mut self,
        events: async_channel::Receiver<Evt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const BATCH: usize = 64;
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(evt) = events.recv().await {
                let mut batch = vec![evt];
                while batch.len() < BATCH {
                    let Ok(next) = events.try_recv() else { break };
                    batch.push(next);
                }
                let alive = this
                    .update_in(cx, |app, window, cx| {
                        for evt in batch {
                            app.handle_event(evt, window, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break; // la fenêtre est fermée
                }
            }
        })
        .detach();
    }

    /// Branche la surveillance de fichiers sur le rafraîchissement du statut.
    ///
    /// Le chemin reçu est rattaché au worktree ouvert qui le contient : le
    /// surveillant ne connaît que des fichiers, l'application seule sait à
    /// quel checkout ils appartiennent.
    fn start_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (watcher, changes) = match Watcher::new() {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("surveillance des fichiers indisponible : {e:#}");
                return;
            }
        };
        self.watcher = Some(watcher);

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(path) = changes.recv().await {
                let alive = this
                    .update(cx, |app, cx| app.file_changed(&path, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    // — La note libre d'un worktree ————————————————————————————————

    /// Programme l'écriture de la note libre à chaque frappe.
    ///
    /// Différée d'une seconde, comme les réglages et pour la même raison : une
    /// zone de saisie émet une valeur par frappe, et un coffre se synchronise —
    /// écrire à chaque caractère ferait travailler la synchronisation en
    /// permanence. Pas de bouton « enregistrer » : c'est un bloc-notes, et
    /// devoir le valider serait le meilleur moyen d'y perdre trois phrases.
    fn watch_vault_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputEvent;
        cx.subscribe(&self.journal_input.clone(), |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_journal_save(cx);
            }
        })
        .detach();
        // `subscribe_in` et non `subscribe` : vider un champ demande une
        // fenêtre, que l'événement seul ne porte pas.
        //
        // Entrée ajoute et laisse le champ prêt pour la suivante : une liste se
        // remplit d'une traite, et reprendre la souris entre deux tâches est le
        // geste qu'on cherchait justement à supprimer.
        cx.subscribe_in(
            &self.task_input.clone(),
            window,
            |this, input, event, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let label = input.read(cx).value().to_string();
                this.add_task(&label, cx);
                input.update(cx, |input, cx| input.set_value("", window, cx));
            },
        )
        .detach();
        // Entrée valide, et perdre le focus aussi : `InputState` n'a pas
        // d'événement d'échappement, et abandonner une correction parce qu'on a
        // cliqué à côté serait le plus mauvais des deux défauts.
        cx.subscribe_in(
            &self.task_edit_input.clone(),
            window,
            |this, input, event, _window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    return;
                }
                let label = input.read(cx).value().to_string();
                this.commit_task_edit(&label, cx);
            },
        )
        .detach();
    }

    fn schedule_journal_save(&mut self, cx: &mut Context<Self>) {
        if self.journal_save {
            return;
        }
        self.journal_save = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.journal_save = false;
                this.persist_journal(cx);
            });
        })
        .detach();
    }

    /// Écrit la note libre du worktree affiché, ou l'efface si elle est vide.
    fn persist_journal(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(dir) = self.notes_dir(&worktree, cx) else {
            return;
        };
        let text = self.journal_input.read(cx).value().to_string();
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        // Rien tant que le dossier n'a pas répondu : ce qu'on a en mémoire au
        // démarrage est une page blanche, et l'écrire effacerait la note qu'on
        // n'a pas encore lue.
        if !state.notes_loaded || state.journal == text {
            return;
        }
        let expect = (!state.journal.is_empty()).then(|| crate::files::digest(&state.journal));
        state.journal = text.clone();
        self.git.send(Cmd::WriteVaultFile {
            worktree,
            path: dir.join(crate::ui::vault::NOTES),
            text,
            expect,
        });
    }

    /// Remet dans la zone de saisie la note du worktree affiché.
    ///
    /// Jamais pendant qu'on y écrit : le coffre est relu à chaque écriture —
    /// la nôtre comprise —, et remettre le texte du disque sous les doigts
    /// déplacerait le curseur au milieu d'une phrase. Ce qui arrive d'ailleurs
    /// pendant qu'on tape attendra donc le prochain chargement, et c'est le bon
    /// arbitrage : deux mains sur le même paragraphe n'ont pas de fusion.
    fn sync_journal_input(&mut self, worktree: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.as_deref() != Some(worktree) {
            return;
        }
        if self
            .journal_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            return;
        }
        let Some(text) = self.review.get(worktree).map(|state| state.journal.clone()) else {
            return;
        };
        if self.journal_input.read(cx).value() == text.as_str() {
            return;
        }
        self.journal_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    fn file_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        // Le coffre n'est pas dans le worktree, et ce qui y change ne se lit
        // pas avec `git status` : c'est le dossier lui-même qu'il faut relire.
        if let Some(vault) = self.notes_dir(&active, cx) {
            if path.starts_with(&vault) {
                self.git.send(Cmd::ReadNotes {
                    worktree: active,
                    dir: vault,
                });
                return;
            }
        }
        // Un worktree lié vit à l'intérieur d'un autre chez certains agencements ;
        // ne réagir que pour le checkout affiché évite un rafraîchissement en
        // double et une liste qui clignote.
        if !path.starts_with(&active) {
            return;
        }
        self.request_status(active);
        cx.notify();
    }

    fn handle_event(&mut self, evt: Evt, window: &mut Window, cx: &mut Context<Self>) {
        match evt {
            Evt::RepoOpened {
                main,
                name,
                worktrees,
                opened_at,
            } => {
                if self.repos.iter().any(|r| r.main == main) {
                    return; // déjà ouvert : rouvrir ne doit pas dupliquer
                }
                // À défaut du checkout d'où l'ouverture vient, le premier de la
                // liste, qui est le dépôt principal.
                let first = opened_at.or_else(|| worktrees.first().map(|w| w.path.clone()));
                self.repos.push(RepoState {
                    main: main.clone(),
                    name,
                    worktrees,
                    branches: Vec::new(),
                    default_base: None,
                    collapsed: false,
                });
                Settings::update_global(cx, |s| s.remember_repository(&main));
                self.forget_missing_worktrees(&main, cx);
                self.ensure_wt_project(&main);
                self.git.send(Cmd::LoadBranches { main });
                if self.active.is_none() {
                    if let Some(path) = first {
                        self.select_worktree(path, window, cx);
                    }
                }
            }
            Evt::Worktrees { main, worktrees } => {
                if let Some(repo) = self.repos.iter_mut().find(|r| r.main == main) {
                    repo.worktrees = worktrees;
                }
                // git vient d'énumérer : c'est le seul moment où la liste est
                // sûre, donc le seul où oublier une entrée est sans risque.
                self.forget_missing_worktrees(&main, cx);
                // Un worktree créé pour une issue attend son prompt : c'est le
                // seul signal qui dise que `wt` a fini ses hooks.
                self.deliver_awaited_agent(window, cx);
                // Le worktree actif peut avoir été retiré sous nos pieds.
                if let Some(active) = self.active.clone() {
                    if !self.worktree_exists(&active) {
                        self.active = None;
                        self.review.remove(&active);
                        self.terminals.remove(&active);
                        if let Some(first) = self.first_worktree() {
                            self.select_worktree(first, window, cx);
                        }
                    }
                }
            }
            Evt::Status { worktree, status } => {
                self.pending_status.remove(&worktree);
                let base = self.default_base_for(&worktree);
                self.ensure_review(&worktree, cx);
                let state = self.review.entry(worktree.clone()).or_default();
                state.status = status;
                if state.base.is_none() {
                    state.base = base;
                }
                // Les listes dépendent du statut : un fichier qu'on vient
                // d'indexer ne doit pas rester affiché du mauvais côté. On les
                // **redemande** sans les vider : effacer ce qui est à l'écran
                // avant d'avoir de quoi le remplacer fait clignoter la liste à
                // chaque rafraîchissement, et il en arrive un par écriture de
                // fichier.
                let stale: Vec<DiffRange> = state.files.keys().cloned().collect();
                for range in stale {
                    if state.pending_files.insert(range.clone()) {
                        self.git.send(Cmd::LoadDiffFiles {
                            worktree: worktree.clone(),
                            range,
                        });
                    }
                }
            }
            Evt::DiffFiles {
                worktree,
                range,
                files,
            } => {
                let Some(state) = self.review.get_mut(&worktree) else {
                    return;
                };
                state.pending_files.remove(&range);
                // Le fichier affiché venait-il de ce domaine, et y est-il
                // encore ? S'il a disparu — indexé, jeté, committé — laisser
                // son diff à l'écran ferait relire un état qui n'existe plus.
                let gone = state.range == range
                    && state
                        .selected
                        .as_ref()
                        .is_some_and(|path| !files.iter().any(|f| &f.path == path));
                // Une relecture périmée s'en va avec la liste qui l'infirme :
                // le fichier a disparu du domaine, ou il a rechangé depuis
                // qu'on l'a coché. La garder ferait dire « relu » d'un contenu
                // que personne n'a lu.
                let before = state.reviewed.len();
                state.reviewed.retain(|item| {
                    item.range != range
                        || files.iter().any(|file| {
                            file.path == item.path
                                && file.added == item.added
                                && file.removed == item.removed
                        })
                });
                let pruned = state.reviewed.len() != before;
                state.files.insert(range, files);
                if gone {
                    state.selected = None;
                    state.diff = None;
                    state.diff_selection = None;
                }
                if pruned {
                    self.persist_notes(&worktree, cx);
                }
            }
            Evt::FileDiff {
                worktree,
                path,
                diff,
            } => {
                // Le thème est lu avant l'emprunt mutable de l'état : la
                // coloration en dépend, et `cx.theme()` emprunte `cx`.
                let theme = cx.theme().highlight_theme.clone();
                let split = Settings::global(cx).diff_split;
                let mut jumped = None;
                let mut note = None;
                if let Some(state) = self.review.get_mut(&worktree) {
                    if state.selected.as_deref() == Some(path.as_path()) {
                        let rendered = std::rc::Rc::new(Rendered::new(&path, diff, &theme));
                        // La flèche qui a ouvert ce fichier attend une
                        // modification, pas le haut du fichier.
                        if let Some(jump) = state.pending_jump.take() {
                            let headers = rendered.headers(split);
                            jumped = match jump {
                                Jump::First => headers.first().copied(),
                                Jump::Last => headers.last().copied(),
                            };
                            state.diff_selection = jumped.map(|row| (row, row));
                        }
                        note = state.pending_note.take();
                        state.diff = Some(rendered);
                        // Les occurrences portent des décalages dans un texte
                        // qui vient d'être remplacé.
                        self.diff_search.valid = false;
                    }
                }
                // Les lignes annotées se déduisent du diff qui vient
                // d'arriver : c'est le seul moment où le calcul a lieu, et
                // certainement pas dans le rendu de la liste.
                self.refresh_note_marks(&worktree);
                if let Some(id) = note {
                    self.select_note_rows(id, cx);
                } else if let Some(row) = jumped {
                    self.reveal_diff_row(row, gpui::ScrollStrategy::Top, cx);
                }
            }
            Evt::Summaries { summaries } => {
                self.summaries.extend(summaries);
            }
            Evt::WtProject { main, project } => {
                self.wt_projects.insert(main, project);
            }
            Evt::WtQuestions {
                main,
                slug,
                answers,
                questions,
            } => self.wt_questions_arrived(main, slug, answers, questions, window, cx),
            Evt::WtTask {
                worktree,
                task,
                launch,
            } => self.wt_task_ready(worktree, task, launch, window, cx),
            Evt::WtStates { states } => {
                self.wt_states.extend(states);
            }
            Evt::Issues { issues } => self.issues_arrived(issues, cx),
            Evt::IssueEvent { issue, event } => self.issue_event_arrived(issue, event, cx),
            Evt::ProjectFiles { worktree, files } => self.project_files_arrived(worktree, files),
            Evt::FileContent {
                worktree,
                path,
                content,
            } => self.file_content_arrived(worktree, path, content, window, cx),
            // Le dossier de notes a répondu : c'est lui la source, et ce qu'on
            // avait en mémoire n'était qu'une attente.
            // Le coffre a été écrit : il existe, donc il se surveille. Le
            // relire dans la foulée n'est pas un luxe — c'est le disque qui
            // fait foi, et une écriture refusée (l'agent avait touché au
            // fichier entre-temps) doit rendre la vue à ce qui est vraiment
            // là plutôt qu'à ce qu'on croyait y avoir mis.
            Evt::VaultWritten { worktree } => {
                self.watch_vault(&worktree, cx);
                if let Some(dir) = self.notes_dir(&worktree, cx) {
                    self.git.send(Cmd::ReadNotes { worktree, dir });
                }
            }
            Evt::NotesRead { worktree, files } => {
                let mut notes = Vec::new();
                let mut reviewed = Vec::new();
                let mut todo = None;
                let mut journal = String::new();
                let on_disk = !files.is_empty();
                for (name, text) in files {
                    if name == crate::ui::vault::INDEX {
                        reviewed = crate::ui::vault::parse_index(&text);
                    } else if name == crate::ui::vault::TODO {
                        todo = Some(crate::ui::vault::parse_todo(&text));
                    } else if name == crate::ui::vault::NOTES {
                        journal = text;
                    } else if let Some(note) = crate::ui::vault::parse_note(&text) {
                        notes.push(note);
                    }
                }
                notes.sort_by_key(|note| note.id);
                // La reprise de l'ancien magasin passe par le même chemin que
                // l'installation neuve, comme `migrate_agents` : un fichier
                // d'état antérieur porte ses notes, et elles n'ont personne
                // d'autre pour les écrire dans le dossier. Une seule fois —
                // le magasin est vidé dans la foulée.
                let legacy = Store::global(cx)
                    .worktree(&worktree)
                    .map(|saved| saved.notes.clone())
                    .unwrap_or_default();
                let migrating = !legacy.is_empty();
                if migrating {
                    let known: std::collections::HashSet<u64> =
                        notes.iter().map(|note| note.id).collect();
                    notes.extend(legacy.into_iter().filter(|note| !known.contains(&note.id)));
                    notes.sort_by_key(|note| note.id);
                    if let Some(main) = self.main_of(&worktree) {
                        Store::update_global(cx, |store| {
                            store.worktree_mut(&worktree, &main).notes = Vec::new();
                        });
                    }
                }
                self.ensure_review(&worktree, cx);
                if let Some(state) = self.review.get_mut(&worktree) {
                    // Un identifiant déjà pris par une note du dossier ferait
                    // deux notes du même numéro, et le prompt en désignerait
                    // une pour l'autre.
                    let highest = notes.iter().map(|note| note.id).max().unwrap_or(0);
                    state.next_note = state.next_note.max(highest + 1);
                    state.notes = notes;
                    state.reviewed = reviewed;
                    state.todo = todo;
                    state.journal = journal;
                    state.notes_loaded = true;
                    state.notes_on_disk = on_disk;
                }
                self.refresh_note_marks(&worktree);
                self.sync_journal_input(&worktree, window, cx);
                if migrating {
                    self.persist_notes(&worktree, cx);
                }
                cx.notify();
            }
            Evt::Agents { agents } => {
                let mut next = HashMap::new();
                let mut cpu = HashMap::new();
                for (worktree, processes) in agents {
                    let working = processes.iter().any(|process| {
                        let before = self.agent_cpu.get(&process.pid).copied();
                        // Un processus vu pour la première fois n'a pas de
                        // variation : on le dit en attente, et le prochain
                        // relevé tranchera. L'inverse ferait clignoter la
                        // liste à chaque agent qui démarre.
                        before.is_some_and(|before| {
                            process.cpu.saturating_sub(before) >= AGENT_BUSY_TICKS
                        })
                    });
                    for process in &processes {
                        cpu.insert(process.pid, process.cpu);
                    }
                    let mut programs: Vec<String> = processes
                        .iter()
                        .map(|process| process.program.clone())
                        .collect();
                    programs.sort();
                    programs.dedup();
                    next.insert(
                        worktree,
                        AgentState {
                            count: processes.len(),
                            programs,
                            working,
                        },
                    );
                }
                self.agent_cpu = cpu;
                self.agents = next;
            }
            Evt::History {
                worktree,
                range,
                commits,
                graph,
            } => {
                if let Some(state) = self.review.get_mut(&worktree) {
                    state.history_pending = false;
                    // Une réponse en retard, pour un domaine qu'on ne regarde
                    // plus, remplacerait l'historique par le mauvais.
                    if state.history_range == range {
                        let width = crate::git::history::width(&graph);
                        state.history = Some(std::rc::Rc::new(History {
                            commits,
                            graph,
                            width,
                        }));
                    }
                }
            }
            Evt::Branches {
                main,
                branches,
                default_base,
            } => {
                if let Some(repo) = self.repos.iter_mut().find(|r| r.main == main) {
                    repo.branches = branches;
                    repo.default_base = default_base;
                }
                // Les revues déjà ouvertes attendaient peut-être cette base :
                // le statut arrive avant les branches, et rien ne les
                // rafraîchira une seconde fois.
                let bases: Vec<(PathBuf, Option<String>)> = self
                    .review
                    .keys()
                    .map(|worktree| (worktree.clone(), self.default_base_for(worktree)))
                    .collect();
                let reload: Vec<(PathBuf, DiffRange)> = Vec::new();
                for (worktree, base) in bases {
                    if let Some(state) = self.review.get_mut(&worktree) {
                        if state.base.is_none() {
                            state.base = base;
                        }
                    }
                }
                for (worktree, range) in reload {
                    self.git.send(Cmd::LoadDiffFiles { worktree, range });
                }
                // Après la propagation, pas avant : le sélecteur doit montrer
                // la base retenue, et elle vient d'être décidée.
                self.refresh_base_choices(window, cx);
            }
            Evt::Done {
                worktree,
                action,
                output,
            } => {
                if action == Action::Commit {
                    self.commit_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                let text = if output.trim().is_empty() {
                    tr!(action.success_key())
                } else {
                    SharedString::from(output.trim().to_string())
                };
                self.toast = Some(Toast { text, error: false });
                // L'intégration a abouti : reste à décider du sort du worktree
                // et de sa branche, que `wt` conserve délibérément.
                if action == Action::Integrate {
                    self.offer_cleanup(window, cx);
                }
                // Une opération qui a bougé HEAD change aussi les branches.
                if matches!(
                    action,
                    Action::Commit | Action::Fetch | Action::Pull | Action::Push | Action::Checkout
                ) {
                    if let Some(main) = worktree.as_deref().and_then(|w| self.main_of(w)) {
                        self.git.send(Cmd::LoadBranches { main });
                    }
                }
            }
            Evt::Failed {
                worktree,
                action,
                message,
            } => {
                // Sans cela, un statut qui échoue une fois — dépôt momentanément
                // verrouillé, disque occupé — bloquerait pour de bon tout
                // rafraîchissement ultérieur de ce worktree.
                if let Some(worktree) = worktree.as_ref() {
                    self.pending_status.remove(worktree);
                }
                // Un drapeau d'intégration armé survivrait à l'échec et ferait
                // proposer le ménage à la prochaine réussite, quelle qu'elle
                // soit.
                if action == Action::Integrate {
                    self.integrated = None;
                }
                log::warn!("{action:?} a échoué : {message}");
                self.toast = Some(Toast {
                    text: SharedString::from(message),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    // — Sélection ————————————————————————————————————————————————

    /// Demande un statut, sauf s'il y en a déjà un en vol pour ce worktree.
    fn request_status(&mut self, worktree: PathBuf) {
        if self.pending_status.insert(worktree.clone()) {
            self.git.send(Cmd::RefreshStatus { worktree });
        }
    }

    pub(super) fn select_worktree(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() == Some(path.as_path()) {
            return;
        }
        // Le coffre est surveillé comme le worktree, et pour la même raison :
        // le travail s'y fait ailleurs. Un agent qui coche une tâche dans
        // `TODO.md` ou qui répond dans une note doit se voir tout de suite,
        // sans quoi il faudrait changer de worktree et revenir.
        let previous_vault = self
            .active
            .as_deref()
            .and_then(|previous| self.notes_dir(previous, cx));
        let vault = self.notes_dir(&path, cx);
        if let Some(watcher) = self.watcher.as_mut() {
            if let Some(previous) = self.active.as_deref() {
                watcher.unwatch(previous);
            }
            if let Some(previous) = previous_vault {
                watcher.unwatch_dir(&previous);
            }
            watcher.watch(&path);
            if let Some(vault) = vault {
                watcher.watch_dir(&vault);
            }
        }
        self.active = Some(path.clone());
        self.ensure_review(&path, cx);
        // La note libre suit le worktree affiché : la zone de saisie est
        // unique, et garder le texte du précédent le ferait écrire ici.
        self.sync_journal_input(&path, window, cx);
        self.request_status(path);
        // Chaque worktree a sa base : le sélecteur doit montrer celle-ci, pas
        // celle du worktree qu'on vient de quitter.
        self.refresh_base_choices(window, cx);
        cx.notify();
    }

    /// Ouvre un fichier dans la vue de diff.
    ///
    /// Le domaine vient du panneau d'où le clic part : « Modifications » et
    /// « Revue de branche » montrent le même fichier de deux façons, et c'est
    /// la liste qu'on a cliquée qui décide de laquelle.
    pub(super) fn open_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        range: DiffRange,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.selected = Some(path.clone());
        // Le diff précédent est effacé tout de suite : garder celui d'un autre
        // fichier le temps de la lecture donnerait l'impression que le clic
        // n'a rien fait, puis que le contenu change tout seul.
        state.diff = None;
        state.diff_selection = None;
        // Un saut armé par une flèche ne survit pas à un autre geste : ouvrir
        // un fichier à la souris doit l'ouvrir en haut.
        state.pending_jump = None;
        state.range = range.clone();
        let untracked = state
            .status
            .files
            .iter()
            .any(|f| f.path == path && f.is_untracked());
        self.git.send(Cmd::LoadFileDiff {
            worktree,
            range: range.clone(),
            path: path.clone(),
            context: Settings::global(cx).context_lines(),
            untracked,
        });
        // La liste suit le fichier ouvert : une flèche qui change de fichier
        // le laisserait sinon hors de vue, et on relirait sans savoir où on en
        // est. Le défilement est non strict — un fichier déjà visible ne fait
        // pas sauter la liste sous les yeux.
        self.reveal_file(&range, &path, cx);
        cx.notify();
    }

    /// La poignée de défilement de la liste d'un domaine, créée à la demande.
    ///
    /// Jamais reconstruite : une poignée neuve par frame remettrait la liste en
    /// haut à chaque image.
    pub(super) fn file_scroll(&mut self, range: &DiffRange) -> gpui::UniformListScrollHandle {
        self.file_scroll.entry(range.clone()).or_default().clone()
    }

    /// La poignée de défilement d'un panneau non virtualisé.
    pub(super) fn scroll_of(&mut self, key: &'static str) -> gpui::ScrollHandle {
        self.scrolls.entry(key).or_default().clone()
    }

    /// Redemande le diff du fichier affiché.
    ///
    /// Ce qu'il contient dépend de réglages qui changent en cours de
    /// relecture — le contexte, et « tout le fichier » : il faut alors le
    /// relire, git étant seul à savoir ce que les lignes élidées contenaient.
    pub(super) fn reload_diff(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let (Some(path), range) = (state.selected.clone(), state.range.clone()) else {
            return;
        };
        self.open_file(worktree, path, range, cx);
    }

    /// Demande la liste des fichiers d'un domaine, si elle manque.
    ///
    /// Appelée au rendu du panneau qui l'affiche : c'est lui qui sait ce qu'il
    /// montre, et le charger d'avance coûterait une commande pour un onglet
    /// que personne n'ouvrira.
    pub(super) fn ensure_files(&mut self, range: DiffRange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.files.contains_key(&range) || !state.pending_files.insert(range.clone()) {
            return;
        }
        self.git.send(Cmd::LoadDiffFiles { worktree, range });
        cx.notify();
    }

    pub(super) fn refresh_active(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(main) = self.main_of(&worktree) {
            self.git.send(Cmd::RefreshRepo { main: main.clone() });
            self.git.send(Cmd::LoadBranches { main });
        }
        self.request_status(worktree);
        cx.notify();
    }

    // — Accès à l'état ——————————————————————————————————————————

    /// Remplit le sélecteur de base avec les branches du dépôt courant.
    ///
    /// Les locales d'abord, puis les distantes : c'est l'ordre dans lequel on
    /// les cherche, et `branch::list` les rend déjà ainsi.
    fn refresh_base_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(repo) = self.repo_of(&worktree) else {
            return;
        };
        let choices: Vec<BaseChoice> = repo.branches.iter().map(BaseChoice::of).collect();
        let current = self
            .review
            .get(&worktree)
            .and_then(|state| state.base.clone())
            .map(SharedString::from);

        self.base_select.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(choices), window, cx);
            if let Some(current) = current {
                select.set_selected_value(&current, window, cx);
            }
        });
    }

    /// Change la base de comparaison du worktree courant.
    ///
    /// Le choix est propre au worktree : comparer un worktree d'agent à `dev`
    /// et un autre à la branche d'où il est parti est le cas normal, pas
    /// l'exception.
    pub(super) fn set_base(&mut self, base: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.base.as_deref() == Some(base.as_str()) {
            return;
        }
        state.base = Some(base.clone());
        // Bascule sur la revue de branche : choisir une base en regardant ses
        // modifications en cours n'aurait aucun effet visible, ce qui ferait
        // croire que le sélecteur ne marche pas.
        // Les listes de branche portaient sur l'ancienne base : les oublier
        // les fait redemander au prochain rendu du panneau.
        state
            .files
            .retain(|range, _| !matches!(range, DiffRange::Branch { .. }));
        state
            .pending_files
            .retain(|range| !matches!(range, DiffRange::Branch { .. }));
        // L'historique de branche dépend de la même base.
        if let Some(state) = self.review.get_mut(&worktree) {
            state.history = None;
        }
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    /// Crée l'état d'un worktree, en y remettant ce que le magasin en avait
    /// retenu.
    ///
    /// La base relue **l'emporte** sur celle que git devine : c'est un choix
    /// de l'utilisateur, et le redeviner à chaque lancement était exactement
    /// le manque que le magasin comble. Le repli des dossiers vient du même
    /// endroit, pour la même raison.
    fn ensure_review(&mut self, worktree: &Path, cx: &App) {
        if self.review.contains_key(worktree) {
            return;
        }
        let saved = Store::global(cx).worktree(worktree).cloned();
        let mut state = ReviewState::default();
        if let Some(saved) = saved {
            state.base = saved.base;
            state.collapsed = saved.collapsed.into_iter().collect();
            // Un fichier écrit avant que ce champ existe porte zéro, et une
            // note d'identifiant nul se confondrait avec l'absence de note.
            state.next_note = saved.next_note.max(1);
        }
        self.review.insert(worktree.to_path_buf(), state);
        // Les notes vivent dans un dossier, et un dossier se lit dans un
        // worker : un coffre sur un disque lent figerait la fenêtre le temps
        // d'un `read_dir`.
        if let Some(dir) = self.notes_dir(worktree, cx) {
            self.git.send(Cmd::ReadNotes {
                worktree: worktree.to_path_buf(),
                dir,
            });
        }
    }

    /// Le dossier de notes d'un worktree, sous la racine des réglages.
    pub(super) fn notes_dir(&self, worktree: &Path, cx: &App) -> Option<PathBuf> {
        let main = self.main_of(worktree)?;
        let root = Settings::global(cx).notes_root()?;
        Some(crate::ui::vault::dir_for(&root, &main, worktree))
    }

    /// Écrit dans le magasin ce que le worktree courant a de persistant.
    ///
    /// Un seul point d'écriture plutôt qu'un par champ : les trois tiennent
    /// dans quelques kilo-octets, et les tenir à jour séparément multiplierait
    /// les occasions d'en oublier un.
    pub(super) fn persist_review(&mut self, worktree: &Path, cx: &mut App) {
        let Some(main) = self.main_of(worktree) else {
            return;
        };
        let Some(state) = self.review.get(worktree) else {
            return;
        };
        // Trié : un `HashSet` sérialisé dans un ordre différent à chaque
        // écriture ferait un fichier qui change sans que rien n'ait changé.
        let mut collapsed: Vec<PathBuf> = state.collapsed.iter().cloned().collect();
        collapsed.sort();
        let (base, next_note) = (state.base.clone(), state.next_note);
        Store::update_global(cx, |store| {
            let saved = store.worktree_mut(worktree, &main);
            saved.base = base;
            saved.collapsed = collapsed;
            saved.next_note = next_note;
        });
        self.persist_notes(worktree, cx);
    }

    /// Aligne le dossier de notes du worktree sur ce qu'on a en mémoire.
    ///
    /// Sans minuterie, contrairement aux réglages et au magasin : ce qu'on
    /// envoie est un ordre à un worker, pas une écriture, et une note se
    /// valide au dialogue là où un champ de réglage émet une valeur par
    /// frappe. Le worker, lui, ne réécrit pas un fichier dont le contenu n'a
    /// pas bougé.
    pub(super) fn persist_notes(&mut self, worktree: &Path, cx: &App) {
        let Some(dir) = self.notes_dir(worktree, cx) else {
            return;
        };
        let Some(state) = self.review.get_mut(worktree) else {
            return;
        };
        // Rien tant que le dossier n'a pas répondu : écrire une liste vide
        // effacerait des notes qu'on n'a pas encore lues.
        if !state.notes_loaded {
            return;
        }
        let something = !state.notes.is_empty() || !state.reviewed.is_empty();
        if !something && !state.notes_on_disk {
            return;
        }
        state.notes_on_disk |= something;
        let mut files: Vec<(String, String)> = state
            .notes
            .iter()
            .map(|note| {
                (
                    crate::ui::vault::note_file(note),
                    crate::ui::vault::render_note(note),
                )
            })
            .collect();
        files.push((
            crate::ui::vault::INDEX.to_string(),
            crate::ui::vault::render_index(worktree, &state.reviewed),
        ));
        self.git.send(Cmd::WriteNotes {
            worktree: worktree.to_path_buf(),
            dir,
            files,
        });
    }

    /// (Re)pose la surveillance du coffre du worktree affiché.
    ///
    /// Sans effet s'il est déjà surveillé, et sans effet si le dossier n'existe
    /// pas encore — l'ordre est simplement à renvoyer après l'avoir créé.
    fn watch_vault(&mut self, worktree: &Path, cx: &App) {
        if self.active.as_deref() != Some(worktree) {
            return;
        }
        let Some(vault) = self.notes_dir(worktree, cx) else {
            return;
        };
        if let Some(watcher) = self.watcher.as_mut() {
            watcher.watch_dir(&vault);
        }
    }

    /// La vue en deux colonnes est-elle repliée ?
    ///
    /// La question décide de **quelle liste est affichée**, donc de quelle
    /// poignée porte le défilement : les deux ne sont jamais peintes en même
    /// temps, et viser la mauvaise ferait défiler une liste qui n'est pas là.
    pub(super) fn diff_wrapped(&self, cx: &App) -> bool {
        let settings = Settings::global(cx);
        settings.diff_split && settings.diff_wrap
    }

    /// La poignée que gpui anime réellement pour le diff affiché.
    pub(super) fn diff_base_handle(&self, cx: &App) -> gpui::ScrollHandle {
        use crate::ui::scroll::Scrollable;
        if self.diff_wrapped(cx) {
            self.diff_wrap_scroll.base()
        } else {
            self.diff_scroll.base()
        }
    }

    /// Amène une entrée du diff dans la vue.
    pub(super) fn reveal_diff_row(&self, row: usize, strategy: gpui::ScrollStrategy, cx: &App) {
        if self.diff_wrapped(cx) {
            self.diff_wrap_scroll.scroll_to_item(row, strategy);
        } else {
            self.diff_scroll.scroll_to_item(row, strategy);
        }
    }

    /// Marque des fichiers relus, ou rend leur relecture.
    ///
    /// Le volume est retenu au moment du clic : c'est lui qui périme la coche
    /// quand un agent réécrit le fichier.
    pub(super) fn set_reviewed(
        &mut self,
        worktree: PathBuf,
        range: DiffRange,
        paths: Vec<PathBuf>,
        reviewed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        let volumes: HashMap<&PathBuf, (usize, usize)> = state
            .files
            .get(&range)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|file| (&file.path, (file.added, file.removed)))
            .collect();
        for path in &paths {
            state
                .reviewed
                .retain(|item| item.range != range || item.path != *path);
            if reviewed {
                let (added, removed) = volumes.get(path).copied().unwrap_or((0, 0));
                state.reviewed.push(crate::ui::vault::Reviewed {
                    range: range.clone(),
                    path: path.clone(),
                    added,
                    removed,
                });
            }
        }
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    /// Oublie ce qu'on retenait de worktrees que git ne liste plus.
    fn forget_missing_worktrees(&mut self, main: &Path, cx: &mut App) {
        let Some(repo) = self.repos.iter().find(|r| r.main == main) else {
            return;
        };
        let alive: Vec<PathBuf> = repo.worktrees.iter().map(|w| w.path.clone()).collect();
        let main = main.to_path_buf();
        Store::update_global(cx, |store| store.forget_missing(&main, &alive));
    }

    /// Base de comparaison de la revue courante, si elle en a une.
    pub(super) fn active_review(&self) -> Option<&ReviewState> {
        self.active.as_ref().and_then(|p| self.review.get(p))
    }

    pub(super) fn active_review_mut(&mut self) -> Option<&mut ReviewState> {
        let path = self.active.clone()?;
        self.review.get_mut(&path)
    }

    /// Dit quelque chose dans la barre d'état. Pour les gestes qui réussissent
    /// sans rien changer à l'écran — copier, par exemple — c'est le seul
    /// accusé de réception qu'on puisse donner.
    pub(super) fn announce(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.toast = Some(Toast { text, error: false });
        cx.notify();
    }

    pub(super) fn main_of(&self, worktree: &Path) -> Option<PathBuf> {
        self.repos
            .iter()
            .find(|r| r.worktrees.iter().any(|w| w.path == worktree))
            .map(|r| r.main.clone())
    }

    pub(super) fn repo_of(&self, worktree: &Path) -> Option<&RepoState> {
        self.repos
            .iter()
            .find(|r| r.worktrees.iter().any(|w| w.path == worktree))
    }

    pub(super) fn active_worktree(&self) -> Option<&Worktree> {
        let path = self.active.as_deref()?;
        self.repos
            .iter()
            .flat_map(|r| r.worktrees.iter())
            .find(|w| w.path == path)
    }

    fn worktree_exists(&self, path: &Path) -> bool {
        self.repos
            .iter()
            .any(|r| r.worktrees.iter().any(|w| w.path == path))
    }

    fn first_worktree(&self) -> Option<PathBuf> {
        self.repos
            .iter()
            .flat_map(|r| r.worktrees.iter())
            .next()
            .map(|w| w.path.clone())
    }

    /// Base de comparaison d'un worktree : la branche d'intégration du dépôt,
    /// sauf quand c'est justement celle qui y est déployée — comparer une
    /// branche à elle-même ne montre rien.
    fn default_base_for(&self, worktree: &Path) -> Option<String> {
        let repo = self.repo_of(worktree)?;
        let base = repo.default_base.as_deref()?;
        let current = repo
            .worktrees
            .iter()
            .find(|w| w.path == worktree)
            .and_then(|w| w.branch.as_deref());
        (Some(base) != current).then(|| base.to_string())
    }

    // — Rendu ——————————————————————————————————————————————————

    fn render_topbar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let worktree = self.active_worktree();
        let label = worktree
            .map(|w| w.label())
            .unwrap_or_else(|| tr!("no-worktree").to_string());
        let has_active = self.active.is_some();

        h_flex()
            .h(super::theme::toolbar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(self.render_main_menu(cx))
            // Le worktree, et rien d'autre : la branche et sa divergence sont
            // descendues dans la barre d'état, qui ne portait qu'un message
            // épisodique pendant que celle-ci débordait.
            .child(div().font_semibold().text_sm().child(label))
            .child(div().flex_1())
            .child(
                Button::new("fetch")
                    .ghost()
                    .small()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-fetch"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Fetch { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pull")
                    .ghost()
                    .small()
                    .icon(icon("arrow-down-to-line"))
                    .tooltip(tr!("action-pull"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Pull { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("push")
                    .ghost()
                    .small()
                    .icon(icon("arrow-up-from-line"))
                    .tooltip(tr!("action-push"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Push {
                                worktree,
                                force_with_lease: false,
                            });
                        }
                        cx.notify();
                    })),
            )
            .child(Divider::vertical().h(px(16.)))
            // L'historique et les branches sont des onglets du dock, atteints
            // d'un clic sur leur onglet : un bouton de plus ici ferait deux
            // chemins pour le même geste.
            .child(
                Button::new("terminal")
                    .ghost()
                    .small()
                    .icon(icon("square-terminal"))
                    .tooltip(tr!("panel-terminal"))
                    .selected(self.terminal_visible(cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_terminal_panel(window, cx);
                    })),
            )
    }

    /// Le menu de l'application.
    ///
    /// Un seul point d'entrée pour ce qui ne concerne pas le dépôt regardé —
    /// réglages, disposition, sortie — plutôt que des boutons dispersés dans
    /// une barre d'outils qui parle, elle, du worktree courant.
    fn render_main_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        Button::new("main-menu")
            .ghost()
            .small()
            .icon(icon("menu"))
            .tooltip(tr!("menu-title"))
            .dropdown_menu(move |menu, window, cx| {
                let entity = entity.clone();
                let for_reset = entity.clone();
                let for_shortcuts = entity.clone();
                let for_views = entity.clone();
                menu.item(PopupMenuItem::new(tr!("settings-title")).on_click(
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.open_settings(window, cx));
                    },
                ))
                // Les raccourcis sont ce qu'on cherche quand on ne sait plus :
                // ils vivent donc là où l'on va chercher, à côté des réglages,
                // et non dans une aide qu'il faudrait deviner.
                .item(
                    PopupMenuItem::new(tr!("shortcuts-title")).on_click(move |_, window, cx| {
                        for_shortcuts.update(cx, |this, cx| this.open_shortcuts(window, cx));
                    }),
                )
                // Les vues masquées n'ont plus d'onglet : c'est le seul
                // endroit d'où les rappeler, et donc le seul endroit qui dise
                // ce que la fenêtre ne montre pas.
                .submenu(tr!("menu-views"), window, cx, move |menu, _window, _cx| {
                    super::panels::VIEWS
                        .iter()
                        .fold(menu, |menu, &(name, title)| {
                            menu.item(view_toggle(for_views.clone(), name, title))
                        })
                })
                .item(PopupMenuItem::new(tr!("menu-reset-layout")).on_click(
                    move |_, window, cx| {
                        for_reset.update(cx, |this, cx| this.reset_layout(window, cx));
                    },
                ))
                .separator()
                .item(PopupMenuItem::new(tr!("menu-quit")).on_click(|_, _window, cx| cx.quit()))
            })
    }

    /// La barre d'état : où l'on est, et ce qui vient de se passer.
    ///
    /// La branche et sa divergence y vivent parce qu'elles ne changent presque
    /// jamais et n'ont pas à occuper la barre d'outils ; le message, lui, est
    /// épisodique, et une barre qui ne porte que lui reste vide la plupart du
    /// temps.
    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, error) = match &self.toast {
            Some(t) => (t.text.clone(), t.error),
            None => (SharedString::default(), false),
        };
        let branch = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        let (ahead, behind) = self
            .active_review()
            .map(|r| (r.status.ahead, r.status.behind))
            .unwrap_or((0, 0));
        // Sur un disque Windows monté par WSL, la surveillance ne remonte
        // rien : le dire est le seul moyen de distinguer « rien n'a changé »
        // de « Claudhub ne voit plus rien ». Le calcul est refait à chaque frame
        // parce qu'il ne coûte qu'une comparaison de composants de chemin,
        // l'appartenance à WSL étant retenue une fois pour toutes.
        let unwatched = self
            .active
            .as_deref()
            .is_some_and(crate::runtime::watch::on_windows_filesystem);
        let muted = cx.theme().muted_foreground;

        h_flex()
            .h(super::theme::row_height(cx))
            .w_full()
            .px_2()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .text_xs()
            .text_color(muted)
            .when(self.active.is_some(), |el| {
                el.child(icon("git-branch").xsmall())
                    .child(div().max_w(px(220.)).truncate().child(branch))
                    // Le retard avant l'avance : c'est ce qu'il faut intégrer
                    // avant de pouvoir pousser.
                    .when(behind > 0, |el| el.child(format!("↓{behind}")))
                    .when(ahead > 0, |el| el.child(format!("↑{ahead}")))
                    .child(Divider::vertical().h(px(12.)))
            })
            // Une opération à mi-chemin passe avant tout le reste : tant
            // qu'elle dure, ce que la revue affiche n'est pas ce qu'on croit.
            .children(self.render_pending_bar(cx))
            .when(unwatched, |el| {
                el.child(
                    h_flex()
                        .gap_1()
                        .text_color(cx.theme().warning)
                        .child(icon("triangle-alert").xsmall())
                        .child(tr!("watch-windows-filesystem")),
                )
                .child(Divider::vertical().h(px(12.)))
            })
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .when(error, |el| el.text_color(cx.theme().danger))
                    .child(text),
            )
    }
}

impl Focusable for ClaudhubApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ClaudhubApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            // Le mode vim se lit au rendu et non à la construction : le
            // contexte est ce qui allume ses liaisons, et le réglage se change
            // en cours de route.
            .key_context(super::shortcuts::context(Settings::global(cx).vim_mode))
            .track_focus(&self.focus)
            .on_action(cx.listener(super::shortcuts::refresh))
            .on_action(cx.listener(super::shortcuts::new_terminal))
            .on_action(cx.listener(super::shortcuts::close_terminal))
            .on_action(cx.listener(super::shortcuts::toggle_terminal))
            .on_action(cx.listener(super::shortcuts::next_terminal))
            .on_action(cx.listener(super::shortcuts::commit))
            .on_action(cx.listener(super::shortcuts::open_settings))
            .on_action(cx.listener(super::shortcuts::zoom_in))
            .on_action(cx.listener(super::shortcuts::zoom_out))
            .on_action(cx.listener(super::shortcuts::zoom_reset))
            .on_action(cx.listener(super::shortcuts::copy_diff))
            .on_action(cx.listener(super::shortcuts::copy_diff_patch))
            .on_action(cx.listener(super::shortcuts::select_whole_diff))
            .on_action(cx.listener(super::shortcuts::previous_line))
            .on_action(cx.listener(super::shortcuts::next_line))
            .on_action(cx.listener(super::shortcuts::extend_up))
            .on_action(cx.listener(super::shortcuts::extend_down))
            .on_action(cx.listener(super::shortcuts::previous_hunk))
            .on_action(cx.listener(super::shortcuts::next_hunk))
            .on_action(cx.listener(super::shortcuts::previous_file))
            .on_action(cx.listener(super::shortcuts::next_file))
            .on_action(cx.listener(super::shortcuts::toggle_diff_split))
            .on_action(cx.listener(super::shortcuts::toggle_whole_file))
            .on_action(cx.listener(super::shortcuts::annotate_selection))
            .on_action(cx.listener(super::shortcuts::ask_agent))
            .on_action(cx.listener(super::shortcuts::send_notes))
            .on_action(cx.listener(super::shortcuts::save_file))
            .on_action(cx.listener(super::shortcuts::find))
            .on_action(cx.listener(super::shortcuts::close_find))
            .on_action(cx.listener(super::shortcuts::find_next))
            .on_action(cx.listener(super::shortcuts::find_previous))
            .on_action(cx.listener(super::shortcuts::explorer_up))
            .on_action(cx.listener(super::shortcuts::explorer_down))
            .on_action(cx.listener(super::shortcuts::explorer_left))
            .on_action(cx.listener(super::shortcuts::explorer_right))
            .on_action(cx.listener(super::shortcuts::explorer_open))
            .on_action(cx.listener(super::shortcuts::explorer_home))
            .on_action(cx.listener(super::shortcuts::explorer_end))
            .on_action(cx.listener(super::shortcuts::show_shortcuts))
            .on_action(cx.listener(super::shortcuts::toggle_sidebar))
            .on_action(cx.listener(super::shortcuts::previous_terminal))
            .on_action(cx.listener(super::shortcuts::select_worktree))
            .on_action(cx.listener(super::shortcuts::fetch))
            .on_action(cx.listener(super::shortcuts::pull))
            .on_action(cx.listener(super::shortcuts::push))
            .on_action(cx.listener(super::shortcuts::toggle_stage))
            .on_action(cx.listener(super::shortcuts::toggle_review_tree))
            .on_action(cx.listener(super::shortcuts::diff_start))
            .on_action(cx.listener(super::shortcuts::diff_end))
            .on_action(cx.listener(super::shortcuts::diff_page_up))
            .on_action(cx.listener(super::shortcuts::diff_page_down))
            .on_action(cx.listener(super::shortcuts::close_editor))
            .size_full()
            // La gouttière et non le fond : ce qui se voit entre deux panneaux
            // — les poignées de redimensionnement, une zone repliée — est le
            // plan sur lequel les cartes sont posées, pas la surface d'une
            // carte.
            .bg(super::theme::gutter(cx))
            .text_color(cx.theme().foreground)
            .child(self.render_topbar(window, cx))
            // Le même souffle que le fork met entre les cartes, autour d'elles :
            // sans ce rembourrage, les zones touchent les bords de la fenêtre,
            // la barre du haut et la barre d'état, et les cartes ne respirent
            // que de l'intérieur.
            .child(div().flex_1().min_h_0().p(px(4.)).child(self.dock.clone()))
            .child(self.render_status_bar(cx))
            // Les couches de gpui-component doivent être ré-émises par la vue
            // racine, sinon dialogues et notifications ne s'affichent nulle
            // part.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

impl ClaudhubApp {
    /// Ouvre un dialogue à une seule ligne de saisie.
    ///
    /// L'`InputState` est créé ici et capturé par la fermeture : une entité
    /// recréée à chaque frame perdrait le curseur, la sélection et le texte
    /// dès le premier caractère.
    pub(super) fn open_text_dialog(
        &mut self,
        title: SharedString,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.open_text_dialog_with(title, placeholder, "", window, cx, on_ok)
    }

    /// La même chose, le champ déjà rempli.
    ///
    /// « Nouveau fichier ici » a besoin du dossier sous le curseur : le
    /// retaper à chaque fois est ce qui fait qu'on ne se sert pas du geste.
    pub(super) fn open_text_dialog_with(
        &mut self,
        title: SharedString,
        placeholder: SharedString,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let value = value.into();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
        });
        let entity = cx.entity();
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, entity, on_ok) = (input.clone(), entity.clone(), on_ok.clone());
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .close_button(false)
                .child(gpui_component::input::Input::new(&input))
                // La fenêtre est passée à la fermeture : ce qu'on lance
                // ensuite — ouvrir un terminal, y livrer un texte — en a
                // besoin, et la reprendre après coup demanderait une frame
                // d'écart avec le geste.
                .on_ok(move |_, window, cx| {
                    let value = input.read(cx).value().to_string();
                    entity.update(cx, |this, cx| on_ok(this, value, window, cx));
                    true
                })
        });
    }
}

impl ClaudhubApp {
    pub(super) fn active_path(&self) -> Option<PathBuf> {
        self.active.clone()
    }

    /// Remet les panneaux à leur place d'origine.
    ///
    /// La sortie de secours d'un système où l'on peut tout déplacer : un
    /// panneau glissé hors de vue n'a sinon aucun moyen de revenir.
    pub(super) fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();

        let panels = DefaultPanels {
            sidebar: panel_handle(cx.new(|cx| SidebarPanel::new(&this, cx))),
            branches: panel_handle(cx.new(|cx| BranchesPanel::new(&this, cx))),
            files: panel_handle(cx.new(|cx| FilesPanel::new(&this, cx))),
            changes: panel_handle(cx.new(|cx| ChangesPanel::new(&this, cx))),
            branch: panel_handle(cx.new(|cx| BranchPanel::new(&this, cx))),
            history: panel_handle(cx.new(|cx| HistoryPanel::new(&this, cx))),
            notes: panel_handle(cx.new(|cx| NotesPanel::new(&this, cx))),
            sentry: panel_handle(cx.new(|cx| SentryPanel::new(&this, cx))),
            conflicts: panel_handle(cx.new(|cx| ConflictsPanel::new(&this, cx))),
            diff: panel_handle(cx.new(|cx| DiffPanel::new(&this, cx))),
            terminal: panel_handle(cx.new(|cx| TerminalPanel::new(&this, cx))),
        };
        self.dock.update(cx, |area, cx| {
            install_default_layout(area, panels, window, cx);
        });
        self.schedule_layout_save(cx);
        cx.notify();
    }

    pub(super) fn terminal_visible(&self, _cx: &App) -> bool {
        self.panel_visible(super::panels::TerminalPanel::NAME)
    }

    pub(super) fn show_terminal_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_panel_visible(super::panels::TerminalPanel::NAME, true, cx);
    }

    /// Une vue est visible tant qu'on ne l'a pas masquée.
    ///
    /// La question se pose par la négative : un panneau qu'on vient d'ajouter
    /// s'affiche sans que rien n'ait à le déclarer, et un nom inconnu du
    /// fichier de réglages — un panneau renommé — ne cache plus rien.
    pub(super) fn panel_visible(&self, name: &str) -> bool {
        !self.hidden_panels.contains(name)
    }

    pub(super) fn set_panel_visible(&mut self, name: &str, visible: bool, cx: &mut Context<Self>) {
        let changed = if visible {
            self.hidden_panels.remove(name)
        } else {
            self.hidden_panels.insert(name.to_string())
        };
        if !changed {
            return;
        }
        // Trié avant d'être écrit, comme les replis du magasin : un ensemble
        // sérialisé dans un ordre différent à chaque fois ferait un fichier de
        // réglages qui change sans que rien n'ait changé.
        let mut hidden: Vec<String> = self.hidden_panels.iter().cloned().collect();
        hidden.sort();
        Settings::update_global(cx, |s| s.hidden_panels = hidden);
        cx.notify();
    }

    pub(super) fn toggle_panel(&mut self, name: &str, cx: &mut Context<Self>) {
        let visible = self.panel_visible(name);
        self.set_panel_visible(name, !visible, cx);
    }

    /// La zone que le zoom au clavier vise.
    ///
    /// Le focus décide : un terminal qu'on regarde se grossit tout seul, et
    /// partout ailleurs c'est le code relu qu'on veut agrandir. Demander à
    /// l'utilisateur de désigner une zone avant de zoomer serait un geste de
    /// plus pour une intention qui n'a jamais d'ambiguïté.
    fn zoom_zone(&self, window: &Window, cx: &App) -> crate::ui::settings::Zoom {
        let terminal_focused = self
            .active
            .as_ref()
            .and_then(|worktree| self.terminals.get(worktree))
            .is_some_and(|group| group.read(cx).is_focused(window, cx));
        if terminal_focused {
            crate::ui::settings::Zoom::Terminal
        } else {
            crate::ui::settings::Zoom::Diff
        }
    }

    pub(super) fn zoom(&mut self, steps: f32, window: &mut Window, cx: &mut Context<Self>) {
        let zone = self.zoom_zone(window, cx);
        Settings::update_global(cx, |s| {
            s.zoom(zone, steps);
        });
        cx.notify();
    }

    pub(super) fn reset_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let zone = self.zoom_zone(window, cx);
        Settings::update_global(cx, |s| {
            s.reset_zoom(zone);
        });
        cx.notify();
    }

    pub(super) fn toggle_terminal_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(super::panels::TerminalPanel::NAME, cx);
    }

    /// Affiche ou masque la zone de gauche — dépôts, branches, fichiers.
    ///
    /// `toggle_dock` ne notifie que le dock intérieur, et c'est l'aire qu'on
    /// observe pour enregistrer la disposition : sans ce `notify`, la fenêtre
    /// rouvrirait avec la zone dans l'état d'avant le geste.
    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |area, cx| {
            area.toggle_dock(gpui_component::dock::DockPlacement::Left, window, cx);
            cx.notify();
        });
    }

    /// Les worktrees dans l'ordre où la barre latérale les affiche.
    ///
    /// Les replis n'y changent rien : `Ctrl+3` doit désigner le même worktree
    /// qu'on ait replié son dépôt ou non, sans quoi le raccourci ne serait
    /// mémorisable que dans un seul état de la liste.
    fn worktrees_in_order(&self) -> Vec<PathBuf> {
        self.repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter().map(|w| w.path.clone()))
            .collect()
    }

    pub(super) fn select_worktree_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.worktrees_in_order().into_iter().nth(index) {
            self.select_worktree(path, window, cx);
        }
    }

    pub(super) fn fetch(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Fetch { worktree });
            cx.notify();
        }
    }

    pub(super) fn pull(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Pull { worktree });
            cx.notify();
        }
    }

    pub(super) fn push(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Push {
                worktree,
                force_with_lease: false,
            });
            cx.notify();
        }
    }

    /// Coche ou décoche le fichier ouvert, comme un clic sur sa case.
    ///
    /// Le statut est la seule source qui distingue l'index du répertoire de
    /// travail : un fichier absent de sa liste n'est pas indexable — c'est un
    /// fichier de commit, pas une modification en cours.
    pub(super) fn toggle_stage_of_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(path) = state.selected.clone() else {
            return;
        };
        let Some(file) = state.status.files.iter().find(|file| file.path == path) else {
            return;
        };
        let staged = file.is_staged();
        self.set_staged(worktree, vec![path], !staged, cx);
    }
}
