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
    divider::Divider,
    dock::{DockArea, DockItem},
    h_flex,
    input::InputState,
    menu::{DropdownMenu, PopupMenuItem},
    select::{SearchableVec, SelectEvent, SelectState},
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
    BranchPanel, BranchesPanel, ChangesPanel, DiffPanel, HistoryPanel, NotesPanel, SidebarPanel,
    TerminalPanel,
};
use crate::ui::settings::Settings;
use crate::ui::store::Store;
use crate::ui::terminal_view::TerminalGroup;

/// Hauteur d'origine du panneau des terminaux.
const TERMINAL_HEIGHT: gpui::Pixels = px(280.);

/// Version de la disposition enregistrée. À incrémenter quand les panneaux
/// changent de nom ou de nature, pour que gpui-component écarte une
/// disposition qu'il ne saurait plus reconstruire.
const LAYOUT_VERSION: usize = 4;

/// Les panneaux de la disposition par défaut.
struct DefaultPanels {
    sidebar: Arc<dyn gpui_component::dock::PanelView>,
    branches: Arc<dyn gpui_component::dock::PanelView>,
    changes: Arc<dyn gpui_component::dock::PanelView>,
    branch: Arc<dyn gpui_component::dock::PanelView>,
    history: Arc<dyn gpui_component::dock::PanelView>,
    notes: Arc<dyn gpui_component::dock::PanelView>,
    diff: Arc<dyn gpui_component::dock::PanelView>,
    terminal: Arc<dyn gpui_component::dock::PanelView>,
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
    weak_dock: &gpui::WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    use crate::ui::layout::split;

    // Les dépôts et les branches l'un au-dessus de l'autre, et non en onglets :
    // on choisit un worktree *puis* on regarde ses branches, et devoir passer
    // de l'un à l'autre pour cela est un aller-retour de trop. Un tiers pour
    // les branches, mesuré sur la fenêtre plutôt que fixé en pixels : la
    // proportion tient d'un écran à l'autre, là où un nombre de pixels
    // occuperait la moitié d'une petite fenêtre.
    // Les deux tailles sont données explicitement : un `None` laisse la pile
    // partager la hauteur en parts égales, et la proportion demandée passe à
    // la trappe.
    let height = window.viewport_size().height.max(px(600.));
    let third = height / 3.;
    area.set_left_dock(
        split(
            gpui::Axis::Vertical,
            vec![
                DockItem::tabs(vec![panels.sidebar], weak_dock, window, cx),
                DockItem::tabs(vec![panels.branches], weak_dock, window, cx),
            ],
            vec![Some(height - third), Some(third)],
            weak_dock,
            window,
            cx,
        ),
        Some(px(280.)),
        true,
        window,
        cx,
    );
    area.set_center(
        split(
            gpui::Axis::Vertical,
            vec![
                split(
                    gpui::Axis::Horizontal,
                    vec![
                        // Les trois façons de choisir quoi relire : ce qui
                        // change maintenant, ce que la branche a écrit, ce qui
                        // est déjà committé. Des onglets et non des panneaux
                        // côte à côte — ils répondent à la même question, et se
                        // glissent ailleurs d'un geste si l'on préfère les voir
                        // ensemble.
                        DockItem::tabs(
                            vec![
                                panels.changes,
                                panels.branch,
                                panels.history,
                                // La relecture est le quatrième point de vue
                                // sur le même travail : ce qu'on a eu à en
                                // dire. Elle se lit au même endroit que ce
                                // qu'elle commente.
                                panels.notes,
                            ],
                            weak_dock,
                            window,
                            cx,
                        ),
                        DockItem::tabs(vec![panels.diff], weak_dock, window, cx),
                    ],
                    vec![Some(px(420.)), None],
                    weak_dock,
                    window,
                    cx,
                ),
                // Les terminaux vivent dans le centre et non dans une zone
                // d'accueil : gpui-component interdit de déplacer le dernier
                // panneau d'une zone, et une zone qui n'en contient qu'un est
                // donc figée. Ici la pile en compte deux — il se glisse.
                DockItem::tabs(vec![panels.terminal], weak_dock, window, cx),
            ],
            vec![Some(height - TERMINAL_HEIGHT), Some(TERMINAL_HEIGHT)],
            weak_dock,
            window,
            cx,
        ),
        window,
        cx,
    );
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
    pub(super) commit_input: Entity<InputState>,
    /// Sélecteur de la base de comparaison. Il est searchable : un dépôt
    /// vivant a des dizaines de branches, et faire défiler une liste de
    /// soixante-dix entrées pour en trouver une dont on connaît le nom est
    /// exactement ce qu'un champ de recherche évite.
    pub(super) base_select: Entity<SelectState<SearchableVec<BaseChoice>>>,
    /// Champ de saisie d'une note. Créé **une fois** : recréé dans un `render`
    /// ou à l'ouverture du dialogue, il perdrait curseur, sélection et texte
    /// dès la première frappe.
    pub(super) note_input: Entity<InputState>,
    /// La note en cours de rédaction : son ancrage, arrêté au moment du geste.
    ///
    /// Il est arrêté là et non à la validation parce que le diff peut changer
    /// pendant qu'on écrit — un agent travaille pendant qu'on le relit — et
    /// que la note doit porter sur ce qu'on avait sous les yeux.
    pub(super) note_draft: Option<crate::ui::notes_view::NoteDraft>,
    /// Le panneau des notes ne montre-t-il que les non traitées.
    pub(super) notes_only_open: bool,
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
    /// Vrai quand une écriture différée de la disposition est déjà programmée.
    layout_save_scheduled: bool,
    /// Le panneau des terminaux est-il affiché.
    ///
    /// Un drapeau et non une zone d'accueil repliable : les terminaux vivent
    /// dans le centre pour rester déplaçables, et c'est `Panel::visible` qui
    /// les fait disparaître.
    pub(super) show_terminal: bool,

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
    pub(super) history_scroll: gpui::UniformListScrollHandle,
    pub(super) branch_scroll: gpui::UniformListScrollHandle,
    /// Défilement des listes de fichiers, **une par domaine** : « Revue » et
    /// « Modifications » sont affichés en même temps, et une seule poignée les
    /// ferait défiler ensemble.
    file_scroll: HashMap<DiffRange, gpui::UniformListScrollHandle>,
    /// Filtre du panneau des branches. Une entité créée une fois : recréée par
    /// frame, elle perdrait le curseur et le texte dès la première frappe.
    pub(super) branch_filter: Entity<InputState>,
    /// Partage entre le graphe et la liste des fichiers du commit choisi.
    pub(super) history_split: Entity<gpui_component::resizable::ResizableState>,
    focus: FocusHandle,
}

impl ClaudhubApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (git, events) = runtime::spawn();

        let commit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder(tr!("commit-placeholder"))
        });

        let branch_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("branch-filter-placeholder")));

        // `auto_grow` plutôt qu'une hauteur fixe : une remarque de relecture
        // fait deux lignes ou dix, et une zone figée oblige à faire défiler ce
        // qu'on est en train d'écrire.
        let note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 8)
                .placeholder(tr!("note-placeholder"))
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
        let dock = cx.new(|cx| DockArea::new("claudhub", Some(LAYOUT_VERSION), window, cx));
        let weak_dock = dock.downgrade();
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
        let changes = cx.new(|cx| ChangesPanel::new(&this, cx));
        let branch = cx.new(|cx| BranchPanel::new(&this, cx));
        let history = cx.new(|cx| HistoryPanel::new(&this, cx));
        let notes = cx.new(|cx| NotesPanel::new(&this, cx));
        let diff = cx.new(|cx| DiffPanel::new(&this, cx));
        let terminal = cx.new(|cx| TerminalPanel::new(&this, cx));

        if !restored {
            let panels = DefaultPanels {
                sidebar: Arc::new(sidebar),
                branches: Arc::new(branches),
                changes: Arc::new(changes),
                branch: Arc::new(branch),
                history: Arc::new(history),
                notes: Arc::new(notes),
                diff: Arc::new(diff),
                terminal: Arc::new(terminal),
            };
            dock.update(cx, |area, cx| {
                install_default_layout(area, panels, &weak_dock, window, cx);
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
            note_draft: None,
            notes_only_open: false,
            toast: None,
            pending_status: std::collections::HashSet::new(),
            dock,
            layout_save_scheduled: false,
            show_terminal: true,
            summaries: HashMap::new(),
            agents: HashMap::new(),
            agent_cpu: HashMap::new(),
            diff_dragging: false,
            watcher: None,
            diff_scroll: gpui::UniformListScrollHandle::new(),
            history_scroll: gpui::UniformListScrollHandle::new(),
            branch_scroll: gpui::UniformListScrollHandle::new(),
            file_scroll: HashMap::new(),
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

    fn file_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
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
                state.files.insert(range, files);
                if gone {
                    state.selected = None;
                    state.diff = None;
                    state.diff_selection = None;
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
                    }
                }
                // Les lignes annotées se déduisent du diff qui vient
                // d'arriver : c'est le seul moment où le calcul a lieu, et
                // certainement pas dans le rendu de la liste.
                self.refresh_note_marks(&worktree);
                if let Some(id) = note {
                    self.select_note_rows(id, cx);
                } else if let Some(row) = jumped {
                    self.diff_scroll
                        .scroll_to_item(row, gpui::ScrollStrategy::Top);
                }
            }
            Evt::Summaries { summaries } => {
                self.summaries.extend(summaries);
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
        if let Some(watcher) = self.watcher.as_mut() {
            if let Some(previous) = self.active.as_deref() {
                watcher.unwatch(previous);
            }
            watcher.watch(&path);
        }
        self.active = Some(path.clone());
        self.ensure_review(&path, cx);
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
            state.notes = saved.notes;
            // Un fichier écrit avant que ce champ existe porte zéro, et une
            // note d'identifiant nul se confondrait avec l'absence de note.
            state.next_note = saved.next_note.max(1);
        }
        self.review.insert(worktree.to_path_buf(), state);
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
        let (base, notes, next_note) = (state.base.clone(), state.notes.clone(), state.next_note);
        Store::update_global(cx, |store| {
            let saved = store.worktree_mut(worktree, &main);
            saved.base = base;
            saved.collapsed = collapsed;
            saved.notes = notes;
            saved.next_note = next_note;
        });
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
            .dropdown_menu(move |menu, _window, _cx| {
                let entity = entity.clone();
                let for_reset = entity.clone();
                menu.item(PopupMenuItem::new(tr!("settings-title")).on_click(
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.open_settings(window, cx));
                    },
                ))
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
    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
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
            .key_context(super::shortcuts::context())
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
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_topbar(window, cx))
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
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
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let entity = cx.entity();
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, entity, on_ok) = (input.clone(), entity.clone(), on_ok.clone());
            dialog
                .title(title.clone())
                .confirm()
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
        let weak_dock = self.dock.downgrade();
        let panels = DefaultPanels {
            sidebar: Arc::new(cx.new(|cx| SidebarPanel::new(&this, cx))),
            branches: Arc::new(cx.new(|cx| BranchesPanel::new(&this, cx))),
            changes: Arc::new(cx.new(|cx| ChangesPanel::new(&this, cx))),
            branch: Arc::new(cx.new(|cx| BranchPanel::new(&this, cx))),
            history: Arc::new(cx.new(|cx| HistoryPanel::new(&this, cx))),
            notes: Arc::new(cx.new(|cx| NotesPanel::new(&this, cx))),
            diff: Arc::new(cx.new(|cx| DiffPanel::new(&this, cx))),
            terminal: Arc::new(cx.new(|cx| TerminalPanel::new(&this, cx))),
        };
        self.dock.update(cx, |area, cx| {
            install_default_layout(area, panels, &weak_dock, window, cx);
        });
        self.schedule_layout_save(cx);
        cx.notify();
    }

    pub(super) fn terminal_visible(&self, _cx: &App) -> bool {
        self.show_terminal
    }

    pub(super) fn show_terminal_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_terminal = true;
        cx.notify();
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
        self.show_terminal = !self.show_terminal;
        cx.notify();
    }
}
