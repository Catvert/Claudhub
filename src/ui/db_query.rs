//! La console SQL.
//!
//! Un éditeur en haut, le résultat en dessous : c'est la console de PhpStorm,
//! et c'est la forme qu'ont toutes celles qu'on a déjà sous les doigts.
//!
//! **Elle prend la place du diff**, comme l'éditeur intégré et pour la même
//! raison : on regarde l'un *ou* l'autre, et un onglet de plus dans le centre
//! serait un aller-retour à chaque requête. C'est aussi ce qui la rend
//! atteignable — le dock de gpui-component ne sait pas activer un onglet
//! depuis le code (`TabPanel::set_active_ix` est privé), si bien qu'un
//! panneau à elle se serait ouvert sans se montrer.
//!
//! **Une seule console à la fois.** Zed en ouvre une par onglet ; ici la place
//! centrale est unique, et deux consoles superposées demanderaient une barre
//! d'onglets à nous. Ouvrir une console sur une autre table remplace la
//! précédente, dont la requête est de toute façon dans l'historique de
//! l'éditeur.
//!
//! ## La fenêtre de résultats
//!
//! Ce qui est affiché n'est pas « la page *n* » mais une **fenêtre** sur le
//! résultat : elle commence à `offset`, elle compte `shown` lignes, et elle
//! **grandit** quand le défilement atteint le bas (`load_more`). Les deux
//! gestes de pagination la déplacent d'un bloc, le défilement la prolonge —
//! et dans les deux cas c'est le même envoi, à un `offset` différent.
//!
//! C'est ce qui permet de parcourir un million de lignes sans jamais en
//! charger plus qu'on n'en a lu, et sans le saut de contexte qu'un « page
//! suivante » impose à l'œil au milieu d'une lecture.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Focusable as _, SharedString, Task, WeakEntity, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{CompletionProvider, Editor, Rope, RopeExt as _},
    menu::{DropdownMenu as _, PopupMenuItem},
    resizable::{resizable_panel, v_resizable, ResizableState},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex, ActiveTheme, Disableable, Sizable,
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::db;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::motion::Axes;
use crate::ui::settings::Settings;

/// Les tailles de fenêtre proposées par la barre.
///
/// Quatre valeurs plutôt qu'un champ de saisie : c'est un ordre de grandeur
/// qu'on choisit — « de quoi voir », « de quoi chercher » — et non un nombre
/// qu'on ajuste à l'unité.
const PAGE_SIZES: [usize; 4] = [100, 500, 1_000, 5_000];

/// Le tri demandé à la console : une colonne, un sens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    /// L'indice de la colonne dans le résultat, qui est aussi son rang dans
    /// l'`ORDER BY` que `db::order_by` écrit.
    pub column: usize,
    pub ascending: bool,
}

/// Un rectangle de cellules, tel que la souris le dessine.
///
/// **Une ancre et un curseur, et non deux coins ordonnés** : c'est l'ancre
/// qu'un Maj+clic garde et le curseur qu'il déplace, et les ordonner à la
/// construction perdrait de quel bout la sélection a commencé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

impl Selection {
    fn cell(row: usize, column: usize) -> Self {
        Self {
            anchor: (row, column),
            cursor: (row, column),
        }
    }

    fn rows(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.0.min(self.cursor.0)..=self.anchor.0.max(self.cursor.0)
    }

    fn columns(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.1.min(self.cursor.1)..=self.anchor.1.max(self.cursor.1)
    }

    fn contains(&self, row: usize, column: usize) -> bool {
        self.rows().contains(&row) && self.columns().contains(&column)
    }

    /// Le nombre de cellules, qui décide de la forme de la copie : une seule
    /// sort telle quelle, plusieurs sortent en colonnes.
    fn count(&self) -> usize {
        (self.rows().count()) * (self.columns().count())
    }
}

/// Ce que la console affiche et ce qu'elle attend.
#[derive(Default)]
pub struct QueryState {
    /// La connexion de la console, quand il y en a une ouverte. `None` : le
    /// panneau central montre le diff.
    pub connection: Option<db::Connection>,
    /// La base courante, celle qu'un `USE` choisirait. `None` pour SQLite,
    /// qui n'en a qu'une.
    pub database: Option<String>,
    /// La requête telle qu'elle est partie.
    ///
    /// C'est elle que rejouent la pagination, le tri et l'export, et non le
    /// texte de l'éditeur : on continue de taper pendant qu'une requête
    /// tourne, et la suite doit porter sur ce qu'on regarde.
    pub sent: Option<String>,
    /// Le tri demandé, appliqué par le moteur autour de `sent`.
    pub sort: Option<Sort>,
    /// La requête se laisse trier — voir `db::can_order`. Connu seulement une
    /// fois les colonnes revenues, d'où sa place ici plutôt qu'à l'envoi.
    pub can_sort: bool,
    /// Le dernier envoi. C'est lui qui identifie la réponse qu'on attend :
    /// changer de page, trier et prolonger rejouent tous le même texte.
    pub request: u64,
    /// L'envoi en cours prolonge la fenêtre au lieu de la remplacer.
    pub appending: bool,
    pub running: bool,
    pub error: Option<SharedString>,
    /// Ce que la fenêtre affichée rapporte, pour la barre d'état et la
    /// pagination. Les lignes, elles, vivent dans le délégué de la table.
    pub offset: usize,
    pub shown: usize,
    pub more: bool,
    pub affected: Option<u64>,
    pub has_columns: bool,
    pub elapsed_ms: u64,
    /// Un export est parti et n'est pas revenu.
    pub exporting: bool,
}

/// Les noms que la console sait compléter.
///
/// Rangé derrière un `RefCell` parce que le fournisseur de complétions est un
/// `Rc<dyn CompletionProvider>` que l'éditeur tient, et qu'il faut pouvoir
/// remplir après coup — l'indexation d'un schéma arrive plusieurs secondes
/// après l'ouverture de la console.
#[derive(Default)]
pub struct SchemaIndex {
    /// La base à laquelle cet index correspond : une console rouverte
    /// ailleurs ne doit pas proposer les tables de la précédente.
    pub database: Option<String>,
    /// `(table, colonnes)`, dans l'ordre du schéma.
    pub tables: Vec<(String, Vec<String>)>,
}

/// Les mots-clés proposés quand aucun schéma n'est indexé — et à côté des
/// noms de tables quand il l'est.
const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "ON",
    "USING",
    "GROUP BY",
    "ORDER BY",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "HAVING",
    "DISTINCT",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "UNION",
    "ALL",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "ALTER",
    "DROP",
    "INDEX",
    "VIEW",
    "EXPLAIN",
];

/// Le résultat d'une requête, tel que la table le lit.
///
/// Le délégué **est** le résultat : la table de gpui-component demande ses
/// cellules une par une au fil du défilement, et lui donner autre chose qu'un
/// accès direct aux lignes ferait une copie par cellule visible et par frame.
#[derive(Default)]
pub struct Results {
    pub rows: db::Rows,
    widths: Vec<gpui::Pixels>,
    mono: Option<gpui::SharedString>,
    /// Le tri en vigueur, qui décide de la flèche des en-têtes.
    sort: Option<Sort>,
    /// Les en-têtes réagissent au clic.
    sortable: bool,
    /// Le résultat continue au-delà de la fenêtre : c'est ce qui autorise le
    /// défilement à en demander la suite.
    more: bool,
    /// Une page est déjà partie. Sans ce garde, chaque frame passée en bas de
    /// la liste en redemanderait une.
    loading: bool,
    /// Le rectangle de cellules sélectionné, s'il y en a un.
    ///
    /// **La sélection est la nôtre et non celle de la table.** Celle de
    /// gpui-component ne connaît qu'une cellule (`selected_cell`), or ce
    /// qu'on copie d'une grille de résultats est presque toujours une colonne
    /// entière ou un bloc. Deux mécanismes se disputeraient le clic et la
    /// couleur de fond ; il n'y en a donc qu'un, et `cell_selectable` reste
    /// éteint.
    selection: Option<Selection>,
    /// Un glissement est en cours : les cellules survolées étendent le
    /// rectangle.
    dragging: bool,
    /// L'application, pour lui reporter un tri ou une demande de suite.
    ///
    /// **Faible**, comme les panneaux du dock : l'application tient la table,
    /// et une référence forte fermerait le cycle.
    app: Option<WeakEntity<ClaudhubApp>>,
}

/// Largeur d'une cellule, déduite du contenu.
///
/// Mesurée sur les cinquante premières lignes seulement : une fenêtre en
/// compte mille, et la colonne la plus large de la fenêtre n'est pas celle
/// qu'on regarde. Bornée des deux côtés — une colonne `id` ne doit pas être un
/// filet, et un `TEXT` de dix mille caractères ne doit pas pousser toutes les
/// autres hors de vue.
fn column_width(rows: &db::Rows, index: usize) -> gpui::Pixels {
    let mut chars = rows
        .columns
        .get(index)
        .map_or(0, |name| name.chars().count());
    for row in rows.rows.iter().take(50) {
        if let Some(Some(value)) = row.get(index) {
            chars = chars.max(value.chars().count());
        }
    }
    px((chars as f32 * 7.5 + 40.).clamp(80., 420.))
}

impl Results {
    fn new(rows: db::Rows, state: &QueryState, cx: &Context<ClaudhubApp>) -> Self {
        let widths = (0..rows.columns.len())
            .map(|index| column_width(&rows, index))
            .collect();
        Self {
            more: rows.more,
            rows,
            widths,
            mono: Some(cx.theme().mono_font_family.clone()),
            sort: state.sort,
            sortable: state.can_sort,
            loading: false,
            selection: None,
            dragging: false,
            app: Some(cx.weak_entity()),
        }
    }

    /// Le tri qu'un clic sur `column` demande : croissant, puis décroissant,
    /// puis plus de tri du tout.
    ///
    /// **La table propose son propre enchaînement et il est ignoré.** Le sien
    /// part du décroissant, ce qui surprend sur une grille de résultats ; et
    /// surtout il vit dans son état à elle, que `refresh` reconstruit à partir
    /// de `column()` à chaque résultat. Une seule des deux mémoires peut faire
    /// foi, et c'est celle de la console — c'est elle qui décide de la requête
    /// envoyée.
    fn next_sort(&self, column: usize) -> Option<Sort> {
        match self.sort {
            Some(sort) if sort.column == column && sort.ascending => Some(Sort {
                column,
                ascending: false,
            }),
            Some(sort) if sort.column == column => None,
            _ => Some(Sort {
                column,
                ascending: true,
            }),
        }
    }

    /// Un clic pose la sélection, ou l'étend si Maj est enfoncée.
    ///
    /// Le glissement s'arme ici : c'est l'enfoncement qui commence une
    /// sélection, pas le relâchement — sans quoi on ne pourrait pas la tirer.
    fn press(&mut self, row: usize, column: usize, extend: bool) {
        self.selection = match (extend, self.selection) {
            (true, Some(selection)) => Some(Selection {
                cursor: (row, column),
                ..selection
            }),
            _ => Some(Selection::cell(row, column)),
        };
        self.dragging = true;
    }

    /// Étend la sélection au passage de la souris. Rend vrai si quelque chose
    /// a bougé — repeindre à chaque pixel parcouru serait du travail pour
    /// rien.
    fn drag_to(&mut self, row: usize, column: usize) -> bool {
        let Some(selection) = self.selection.as_mut() else {
            return false;
        };
        if selection.cursor == (row, column) {
            return false;
        }
        selection.cursor = (row, column);
        true
    }

    /// Tout le résultat chargé, coin à coin.
    fn select_all(&mut self) {
        let (rows, columns) = (self.rows.rows.len(), self.rows.columns.len());
        self.selection = match (rows, columns) {
            (0, _) | (_, 0) => None,
            (rows, columns) => Some(Selection {
                anchor: (0, 0),
                cursor: (rows - 1, columns - 1),
            }),
        };
    }

    /// Le texte de ce qui est sélectionné, prêt pour le presse-papiers.
    ///
    /// **Une cellule seule sort telle quelle** : c'est un identifiant qu'on
    /// va coller dans une autre requête, pas un tableau — l'encadrer de
    /// guillemets serait une corvée de plus à chaque collage. Plusieurs
    /// cellules sortent en colonnes séparées par des tabulations.
    fn selected_text(&self, headers: bool) -> Option<String> {
        let selection = self.selection?;
        if !headers && selection.count() == 1 {
            let (row, column) = selection.anchor;
            return Some(self.cell(row, column).cloned().unwrap_or_default());
        }
        let mut out = String::new();
        if headers {
            out.push_str(&db::tsv_line(selection.columns().map(|column| {
                self.rows.columns.get(column).map(|name| name.as_str())
            })));
        }
        for row in selection.rows() {
            out.push_str(&db::tsv_line(
                selection
                    .columns()
                    .map(|column| self.cell(row, column).map(|value| value.as_str())),
            ));
        }
        Some(out)
    }

    /// Tout le résultat chargé, en-tête compris.
    fn all_text(&self) -> String {
        let mut out = db::tsv_line(self.rows.columns.iter().map(|name| Some(name.as_str())));
        for row in &self.rows.rows {
            out.push_str(&db::tsv_line(row.iter().map(|cell| cell.as_deref())));
        }
        out
    }

    /// Une ligne entière, en-tête compris — c'est ce qu'on relit dans un
    /// message quand on demande « regarde cet enregistrement ».
    fn row_text(&self, row: usize) -> Option<String> {
        let cells = self.rows.rows.get(row)?;
        let mut out = db::tsv_line(self.rows.columns.iter().map(|name| Some(name.as_str())));
        out.push_str(&db::tsv_line(cells.iter().map(|cell| cell.as_deref())));
        Some(out)
    }

    fn cell(&self, row: usize, column: usize) -> Option<&String> {
        self.rows.rows.get(row)?.get(column)?.as_ref()
    }

    /// Reporte un geste de la table à l'application.
    ///
    /// **Différé**, et ce n'est pas une précaution : la table appelle son
    /// délégué au milieu d'un `update` sur elle-même, et l'application répond
    /// en remplaçant ce délégué — donc en réempruntant l'entité qu'on est en
    /// train d'emprunter, ce que gpui refuse par une panique.
    fn report(
        &self,
        cx: &mut App,
        task: impl FnOnce(&mut ClaudhubApp, &mut Context<ClaudhubApp>) + 'static,
    ) {
        let Some(app) = self.app.clone() else { return };
        cx.defer(move |cx| {
            app.update(cx, |this, cx| task(this, cx)).ok();
        });
    }
}

impl TableDelegate for Results {
    fn columns_count(&self, _: &App) -> usize {
        self.rows.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let name = self
            .rows
            .columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string());
        let column = Column::new(name.clone(), name)
            .width(self.widths.get(index).copied().unwrap_or(px(120.)))
            .resizable(true)
            // Le rembourrage passe de la colonne à nos éléments : sans cela,
            // huit pixels de chaque côté d'une cellule ne répondent pas au
            // clic, et une cellule qu'il faut viser n'est pas une cellule
            // qu'on sélectionne.
            .p_0();
        if !self.sortable {
            return column;
        }
        // La flèche est peinte à partir d'ici, et `refresh` relit cette
        // fonction à chaque résultat : c'est ce qui remet l'affichage
        // d'accord avec le tri réellement envoyé.
        match self.sort {
            Some(sort) if sort.column == index && sort.ascending => column.ascending(),
            Some(sort) if sort.column == index => column.descending(),
            _ => column.sort(ColumnSort::Default),
        }
    }

    fn perform_sort(
        &mut self,
        index: usize,
        _: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let next = self.next_sort(index);
        self.report(cx, move |this, cx| this.sort_db_query(next, cx));
    }

    /// L'en-tête entier est cliquable, et pas seulement sa petite flèche.
    ///
    /// C'est le geste de DataGrip et de PhpStorm : on vise le nom de la
    /// colonne. La flèche que la table peint à côté reste le repère de l'état
    /// et déclenche la même chose.
    fn render_th(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self
            .rows
            .columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string());
        let label = div()
            .size_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .child(SharedString::from(name));
        if !self.sortable {
            return label.into_any_element();
        }
        label
            .id(("db-th", index))
            .cursor_pointer()
            .on_click(cx.listener(move |table, _, _window, cx| {
                let next = table.delegate().next_sort(index);
                table
                    .delegate()
                    .report(cx, move |this, cx| this.sort_db_query(next, cx));
            }))
            .into_any_element()
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.rows.rows.get(row).and_then(|row| row.get(column));
        // `NULL` est une valeur et non un texte : l'éteindre est ce qui le
        // distingue de la chaîne « NULL » qu'une colonne peut contenir, et
        // c'est bien deux choses différentes que le résultat porte.
        let (text, null) = match cell {
            Some(Some(value)) => (SharedString::from(value.clone()), false),
            _ => (SharedString::new_static("NULL"), true),
        };
        let selected = self
            .selection
            .is_some_and(|selection| selection.contains(row, column));
        div()
            .size_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .when_some(self.mono.clone(), |el, mono| el.font_family(mono))
            .when(null, |el| {
                el.text_color(cx.theme().muted_foreground).italic()
            })
            .when(selected, |el| el.bg(cx.theme().selection))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |table, event: &gpui::MouseDownEvent, window, cx| {
                    // Un clic dans la grille **prend le focus**, comme un clic
                    // sur une ligne de diff : sans cela le `Ctrl+C` qui suit
                    // part à qui l'avait — le terminal, l'éditeur de requête —
                    // et le contexte `ClaudhubQuery` n'est pas dans la pile.
                    let focus = table.focus_handle(cx);
                    window.focus(&focus, cx);
                    table
                        .delegate_mut()
                        .press(row, column, event.modifiers.shift);
                    cx.notify();
                }),
            )
            // Le bouton est revérifié à chaque déplacement et pas seulement à
            // l'enfoncement : un relâchement hors de la fenêtre n'envoie
            // aucun événement, et la sélection suivrait le curseur après coup.
            .on_mouse_move(
                cx.listener(move |table, event: &gpui::MouseMoveEvent, _window, cx| {
                    if !table.delegate().dragging {
                        return;
                    }
                    if event.pressed_button != Some(gpui::MouseButton::Left) {
                        table.delegate_mut().dragging = false;
                        return;
                    }
                    if table.delegate_mut().drag_to(row, column) {
                        cx.notify();
                    }
                }),
            )
            // Un clic droit **hors** de la sélection la remplace ; dedans, il
            // la garde — sans quoi le menu « copier la sélection » copierait
            // la seule cellule qu'on vient de viser.
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |table, _event, _window, cx| {
                    let inside = table
                        .delegate()
                        .selection
                        .is_some_and(|selection| selection.contains(row, column));
                    if !inside {
                        let delegate = table.delegate_mut();
                        delegate.press(row, column, false);
                        delegate.dragging = false;
                        cx.notify();
                    }
                }),
            )
            .child(text)
    }

    /// Le menu du clic droit.
    ///
    /// Il porte ce qu'on fait d'un résultat qu'on regarde : copier ce qu'on a
    /// sélectionné, la ligne entière, tout, ou l'écrire dans un fichier. Les
    /// entrées portent toutes une icône — c'est la convention de tous les
    /// menus de Claudhub, et une seule entrée sans icône décale les autres.
    fn context_menu(
        &mut self,
        row: usize,
        menu: gpui_component::menu::PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> gpui_component::menu::PopupMenu {
        let Some(app) = self.app.clone() else {
            return menu;
        };
        let selected = self.selection.is_some();
        let (copy, headers, line, all, export) =
            (app.clone(), app.clone(), app.clone(), app.clone(), app);
        menu.item(
            PopupMenuItem::new(tr!("db-copy-selection"))
                .icon(icon("copy"))
                .disabled(!selected)
                .on_click(move |_, _window, cx| {
                    copy.update(cx, |this, cx| this.copy_db_selection(false, cx))
                        .ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-with-headers"))
                .icon(icon("table"))
                .disabled(!selected)
                .on_click(move |_, _window, cx| {
                    headers
                        .update(cx, |this, cx| this.copy_db_selection(true, cx))
                        .ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-copy-row"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    line.update(cx, |this, cx| this.copy_db_row(row, cx)).ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-result"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    all.update(cx, |this, cx| this.copy_db_all(cx)).ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-export"))
                .icon(icon("download"))
                .on_click(move |_, _window, cx| {
                    export.update(cx, |this, cx| this.export_db_csv(cx)).ok();
                }),
        )
    }

    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.rows
            .rows
            .get(row)
            .and_then(|row| row.get(column))
            .cloned()
            .flatten()
            .unwrap_or_default()
    }

    /// Le défilement peut demander la suite tant qu'il en reste.
    fn has_more(&self, _: &App) -> bool {
        self.more && !self.loading
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.report(cx, |this, cx| this.extend_db_rows(cx));
    }
}

impl ClaudhubApp {
    /// Ouvre la console sur une connexion, et éventuellement sur une table.
    ///
    /// Une table donne un `SELECT * FROM …` **et le lance** : « interroger
    /// cette table » qui n'afficherait rien tant qu'on n'a pas trouvé le
    /// bouton serait un geste à moitié fait.
    pub(super) fn start_db_console(
        &mut self,
        connection: db::Connection,
        database: Option<String>,
        table: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // La console prend la place du diff, donc aussi celle de l'éditeur
        // intégré : les trois se disputent le même panneau.
        self.editing = None;
        let changed =
            self.query.connection.as_ref() != Some(&connection) || self.query.database != database;
        self.query.connection = Some(connection.clone());
        self.query.database = database.clone();
        if changed {
            self.query.error = None;
            self.query.sent = None;
            self.query.sort = None;
            self.query.can_sort = false;
            self.set_db_rows(db::Rows::default(), cx);
            self.index_db_schema(&connection, database.as_deref(), cx);
        }
        if let Some(table) = table {
            let quoted = match connection.engine {
                db::Engine::Sqlite => format!("\"{table}\""),
                db::Engine::Mysql => format!("`{table}`"),
            };
            // Sans `LIMIT` : la fenêtre de résultats en tient déjà lieu, et
            // une borne écrite dans le texte survivrait à la requête suivante
            // qu'on écrit par-dessus.
            let sql = format!("SELECT * FROM {quoted};");
            self.db_query_input.update(cx, |state, cx| {
                state.set_value(sql, window, cx);
            });
            self.run_db_query(cx);
        }
        // Ouvrir une console appelle l'écran des bases : le geste vient de
        // l'arbre des schémas, qui y vit, mais aussi du menu d'une table
        // ouverte ailleurs.
        self.enter_workspace(crate::ui::workspace::Workspace::Db, window, cx);
        self.set_panel_visible(crate::ui::panels::ConsolePanel::NAME, true, cx);
        cx.notify();
    }

    /// Referme la console et rend le centre au diff.
    pub(super) fn close_db_console(&mut self, cx: &mut Context<Self>) {
        self.query = QueryState::default();
        self.set_db_rows(db::Rows::default(), cx);
        cx.notify();
    }

    pub(super) fn db_console_open(&self) -> bool {
        self.query.connection.is_some()
    }

    /// Demande les noms que la console complétera.
    ///
    /// C'est la même commande que celle du panneau : si l'arbre a déjà indexé
    /// cette base, la réponse remplit les deux.
    fn index_db_schema(
        &mut self,
        connection: &db::Connection,
        database: Option<&str>,
        _cx: &mut Context<Self>,
    ) {
        let database = match (connection.engine, database) {
            (db::Engine::Sqlite, _) => "main".to_string(),
            (db::Engine::Mysql, Some(name)) => name.to_string(),
            // Sans base choisie, il n'y a pas de schéma à indexer : les
            // complétions se limitent aux mots-clés.
            (db::Engine::Mysql, None) => return,
        };
        self.db_schema.borrow_mut().database = None;
        self.git.send(Cmd::DbAllColumns {
            connection: connection.clone(),
            database,
        });
    }

    /// Range un schéma qui vient d'arriver, s'il est celui de la console.
    pub(super) fn db_schema_indexed(
        &mut self,
        key: &str,
        database: &str,
        columns: &BTreeMap<String, Vec<db::Column>>,
    ) {
        let Some(connection) = self.query.connection.as_ref() else {
            return;
        };
        if connection.key() != key {
            return;
        }
        let expected = match connection.engine {
            db::Engine::Sqlite => "main",
            db::Engine::Mysql => self.query.database.as_deref().unwrap_or_default(),
        };
        if expected != database {
            return;
        }
        let mut index = self.db_schema.borrow_mut();
        index.database = Some(database.to_string());
        index.tables = columns
            .iter()
            .map(|(table, columns)| {
                (
                    table.clone(),
                    columns.iter().map(|column| column.name.clone()).collect(),
                )
            })
            .collect();
    }

    /// Lance ce qu'il y a dans l'éditeur.
    ///
    /// Le tri repart de zéro : il porte sur une colonne du résultat, et rien
    /// ne dit que la nouvelle requête ait la même.
    pub(super) fn run_db_query(&mut self, cx: &mut Context<Self>) {
        let sql = self.db_query_input.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }
        self.query.sent = Some(sql);
        self.query.sort = None;
        self.query.can_sort = false;
        self.send_db_query(0, false, cx);
    }

    /// Trie le résultat, ou lui retire son tri.
    ///
    /// La fenêtre revient à son début : les lignes qui la remplissaient ne
    /// sont plus les premières de rien.
    pub(super) fn sort_db_query(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) {
        if !self.query.can_sort || self.query.sort == sort {
            return;
        }
        self.query.sort = sort;
        // La flèche suit le geste et non la réponse : une requête met parfois
        // une seconde, et un en-tête qui ne bouge pas se lit comme un clic
        // perdu.
        self.db_table.update(cx, |state, cx| {
            state.delegate_mut().sort = sort;
            state.refresh(cx);
        });
        self.send_db_query(0, false, cx);
    }

    /// Déplace la fenêtre.
    pub(super) fn page_db_query(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.send_db_query(offset, false, cx);
    }

    /// Prolonge la fenêtre : le défilement est arrivé en bas.
    pub(super) fn extend_db_rows(&mut self, cx: &mut Context<Self>) {
        if self.query.running || !self.query.more {
            // La table s'est mise en attente avant de nous appeler ; sans
            // cela, elle n'en sortirait plus.
            self.db_table.update(cx, |state, _| {
                state.delegate_mut().loading = false;
            });
            return;
        }
        let next = self.query.offset + self.query.shown;
        self.send_db_query(next, true, cx);
    }

    /// La requête telle qu'elle part vraiment : celle qu'on a lancée, et le
    /// tri qu'on a demandé autour.
    fn effective_sql(&self) -> Option<String> {
        let sent = self.query.sent.clone()?;
        match self.query.sort {
            Some(sort) => Some(db::order_by(&sent, sort.column, sort.ascending).unwrap_or(sent)),
            None => Some(sent),
        }
    }

    fn send_db_query(&mut self, offset: usize, append: bool, cx: &mut Context<Self>) {
        let Some(connection) = self.query.connection.clone() else {
            return;
        };
        let Some(sql) = self.effective_sql() else {
            return;
        };
        let limit = Settings::global(cx).db_page_size.max(1);
        self.query.request += 1;
        self.query.appending = append;
        self.query.running = true;
        self.query.error = None;
        self.git.send(Cmd::DbQuery {
            connection,
            database: self.query.database.clone(),
            sql,
            offset,
            limit,
            request: self.query.request,
        });
        cx.notify();
    }

    /// Le résultat d'une requête.
    ///
    /// Il est **écarté s'il ne répond pas au dernier envoi** : on relance
    /// avant que le précédent soit revenu — en changeant de page, en triant,
    /// en descendant —, et afficher la réponse en retard remplacerait ce qu'on
    /// regarde par ce qu'on ne regarde plus.
    pub(super) fn db_rows_arrived(
        &mut self,
        request: u64,
        rows: crate::runtime::protocol::DbResult<db::Rows>,
        elapsed_ms: u64,
        cx: &mut Context<Self>,
    ) {
        if self.query.request != request {
            return;
        }
        self.query.running = false;
        self.query.elapsed_ms = elapsed_ms;
        match rows {
            Ok(rows) => {
                self.query.error = None;
                let sent = self.query.sent.clone().unwrap_or_default();
                self.query.can_sort = db::can_order(&sent, &rows.columns);
                self.query.affected = rows.affected;
                if self.query.appending {
                    self.query.more = rows.more;
                    self.query.shown += rows.rows.len();
                    self.extend_db_table(rows, cx);
                } else {
                    self.query.offset = rows.offset;
                    self.query.shown = rows.rows.len();
                    self.query.more = rows.more;
                    self.query.has_columns = !rows.columns.is_empty();
                    self.set_db_rows(rows, cx);
                }
            }
            Err(message) => {
                self.query.error = Some(message.into());
                self.query.has_columns = false;
                self.query.can_sort = false;
                self.query.more = false;
                self.set_db_rows(db::Rows::default(), cx);
            }
        }
        cx.notify();
    }

    /// Remplace le contenu de la table.
    ///
    /// La table est une entité créée une fois : la reconstruire à chaque
    /// résultat perdrait les largeurs qu'on vient de régler à la souris et
    /// remettrait le défilement en haut au milieu d'une pagination.
    fn set_db_rows(&mut self, rows: db::Rows, cx: &mut Context<Self>) {
        let results = Results::new(rows, &self.query, cx);
        self.db_table.update(cx, |state, cx| {
            *state.delegate_mut() = results;
            state.refresh(cx);
        });
    }

    /// Ajoute une page sous celles qu'on regarde.
    ///
    /// Les largeurs ne sont **pas** recalculées : elles ont été déduites de la
    /// première page, et les revoir à chaque prolongement ferait bouger les
    /// colonnes sous les yeux de qui défile. `refresh` n'est pas appelé non
    /// plus — il remettrait le défilement en haut, ce qui est exactement le
    /// contraire de ce qu'on vient de demander.
    fn extend_db_table(&mut self, rows: db::Rows, cx: &mut Context<Self>) {
        self.db_table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.rows.extend(rows);
            delegate.more = delegate.rows.more;
            delegate.loading = false;
            cx.notify();
        });
    }

    /// Sélectionne tout le résultat chargé.
    pub(super) fn select_whole_db_result(&mut self, cx: &mut Context<Self>) {
        self.db_table.update(cx, |state, cx| {
            state.delegate_mut().select_all();
            cx.notify();
        });
    }

    /// Copie ce qui est sélectionné.
    ///
    /// Une cellule nulle copie **du vide** et non le mot « NULL » : ce mot est
    /// la façon dont la grille montre l'absence de valeur, et il ne veut plus
    /// rien dire une fois collé ailleurs.
    pub(super) fn copy_db_selection(&mut self, headers: bool, cx: &mut Context<Self>) {
        let Some(text) = self.db_table.read(cx).delegate().selected_text(headers) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copie une ligne entière, avec les noms de colonnes au-dessus.
    pub(super) fn copy_db_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(text) = self.db_table.read(cx).delegate().row_text(row) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copie tout le résultat **chargé** — pas tout le résultat de la
    /// requête, qui est ce que l'export écrit.
    pub(super) fn copy_db_all(&mut self, cx: &mut Context<Self>) {
        let table = self.db_table.read(cx);
        if table.delegate().rows.columns.is_empty() {
            return;
        }
        let text = table.delegate().all_text();
        self.put_on_clipboard(text, cx);
    }

    /// `Ctrl+C` : la sélection s'il y en a une, tout sinon.
    ///
    /// Copier tout faute de sélection est ce que fait déjà la vue de diff :
    /// sur une grille de résultats le geste n'a pas d'autre sens, et refuser
    /// d'agir serait un refus poli sans raison.
    pub(super) fn copy_db_result(&mut self, cx: &mut Context<Self>) {
        if self.db_table.read(cx).delegate().selection.is_some() {
            self.copy_db_selection(false, cx);
        } else {
            self.copy_db_all(cx);
        }
    }

    fn put_on_clipboard(&mut self, text: String, cx: &mut Context<Self>) {
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("db-copied", { n: lines }), cx);
    }

    /// Demande où écrire, puis lance l'export.
    ///
    /// Le sélecteur natif est asynchrone, d'où le `spawn` : c'est le même
    /// chemin que l'ouverture d'un dépôt.
    pub(super) fn export_db_csv(&mut self, cx: &mut Context<Self>) {
        if self.query.exporting || self.query.sent.is_none() {
            return;
        }
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let name = self
            .query
            .connection
            .as_ref()
            .map(|connection| format!("{}.csv", connection.label()))
            .unwrap_or_else(|| "export.csv".to_string());
        let path = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return; // annulé
            };
            let _ = this.update(cx, |this, cx| this.send_db_export(path, cx));
        })
        .detach();
    }

    fn send_db_export(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let (Some(connection), Some(sql)) = (self.query.connection.clone(), self.effective_sql())
        else {
            return;
        };
        // Le fichier est choisi ici et écrit par le worker : sous Windows,
        // c'est donc l'un des rares endroits où un chemin entre par ce
        // monde-ci et doit ressortir dans celui du serveur. Un dossier que la
        // distribution n'atteint pas — un partage réseau — est refusé plutôt
        // qu'exporté nulle part.
        let path = if cfg!(windows) {
            match crate::wslpath::for_server(&path) {
                Some(path) => path,
                None => {
                    self.announce(tr!("db-export-unreachable"), cx);
                    return;
                }
            }
        } else {
            path
        };
        self.query.exporting = true;
        self.git.send(Cmd::DbExport {
            connection,
            database: self.query.database.clone(),
            sql,
            path,
        });
        cx.notify();
    }

    /// Un export est revenu. Le chemin est dit en entier : c'est la seule
    /// chose qu'on ait à retenir pour le retrouver.
    pub(super) fn db_exported(
        &mut self,
        path: std::path::PathBuf,
        rows: crate::runtime::protocol::DbResult<u64>,
        cx: &mut Context<Self>,
    ) {
        self.query.exporting = false;
        match rows {
            Ok(count) => {
                // Le serveur rend le chemin qu'il a écrit, donc un chemin
                // Linux : on le rend à l'utilisateur dans le monde où il l'a
                // choisi, sans quoi il lirait `/mnt/c/…` d'un fichier qu'il
                // ira chercher dans son explorateur.
                let path = if cfg!(windows) {
                    let distro = crate::ui::settings::Settings::global(cx).wsl_distro.clone();
                    crate::wslpath::to_windows(&path, &distro)
                } else {
                    path
                };
                let file = SharedString::from(path.display().to_string());
                self.announce(tr!("db-exported", { n: count, path: file }), cx);
            }
            Err(message) => {
                self.toast = Some(crate::ui::app::Toast {
                    text: SharedString::from(message),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    pub(super) fn render_db_console(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editor = self.db_query_input.clone();
        let split = self.db_split.clone();
        let bar = self.render_console_bar(cx);
        let results = self.render_db_results(window, cx);
        v_flex()
            .id("db-console")
            // Le contexte de la console : c'est lui qui donne `Ctrl+Entrée` à
            // la requête plutôt qu'au commit, et `Ctrl+C` à la grille plutôt
            // qu'au diff.
            .key_context(crate::ui::shortcuts::query_context())
            .size_full()
            .child(bar)
            .child(
                // L'éditeur et la grille se partagent la hauteur, et le
                // partage se règle : on écrit une requête de vingt lignes,
                // puis on lit trois cents lignes de résultat, et aucune
                // proportion figée ne convient aux deux.
                v_resizable("db-split")
                    .with_state(&split)
                    .child(
                        resizable_panel()
                            .size(px(180.))
                            .size_range(px(72.)..px(640.))
                            .child(
                                div()
                                    .size_full()
                                    .overflow_hidden()
                                    .child(Editor::new(&editor).h_full()),
                            ),
                    )
                    .child(resizable_panel().child(results)),
            )
    }

    fn render_console_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = match (&self.query.connection, &self.query.database) {
            (Some(connection), Some(database)) => format!("{} · {database}", connection.label()),
            (Some(connection), None) => connection.label(),
            (None, _) => String::new(),
        };
        let running = self.query.running;
        let has_result = self.query.has_columns && self.query.shown > 0;
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("db-run")
                    .ghost()
                    .xsmall()
                    .icon(icon("play"))
                    .tooltip(tr!("db-run"))
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _window, cx| this.run_db_query(cx))),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(target)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(if self.query.error.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(self.db_status_text()),
            )
            .children(self.render_db_pagination(cx))
            .child(self.render_page_size(cx))
            .child(
                Button::new("db-copy")
                    .ghost()
                    .xsmall()
                    .icon(icon("copy"))
                    .tooltip(tr!("db-copy-result"))
                    .disabled(!has_result)
                    .on_click(cx.listener(|this, _, _window, cx| this.copy_db_result(cx))),
            )
            .child(
                Button::new("db-export")
                    .ghost()
                    .xsmall()
                    .icon(icon("download"))
                    .tooltip(tr!("db-export"))
                    .disabled(!has_result || self.query.exporting)
                    .on_click(cx.listener(|this, _, _window, cx| this.export_db_csv(cx))),
            )
            .child(
                Button::new("db-close")
                    .ghost()
                    .xsmall()
                    .icon(icon("x"))
                    .tooltip(tr!("db-close-console"))
                    .on_click(cx.listener(|this, _, _window, cx| this.close_db_console(cx))),
            )
    }

    /// Ce que la barre dit de la fenêtre affichée.
    fn db_status_text(&self) -> SharedString {
        if self.query.running {
            return tr!("db-running");
        }
        if self.query.error.is_some() {
            return tr!("db-failed");
        }
        let ms = self.query.elapsed_ms;
        if self.query.sent.is_none() {
            return SharedString::default();
        }
        if !self.query.has_columns {
            let affected = self.query.affected.unwrap_or(0);
            return tr!("db-affected", { n: affected, ms: ms });
        }
        if self.query.offset == 0 && !self.query.more {
            return tr!("db-row-count", { n: self.query.shown, ms: ms });
        }
        let first = self.query.offset + 1;
        let last = self.query.offset + self.query.shown;
        tr!("db-row-range", {
            first: first,
            last: last,
            more: if self.query.more { "+" } else { "" },
            ms: ms,
        })
    }

    /// Les deux gestes qui déplacent la fenêtre, quand le résultat en dépasse.
    fn render_db_pagination(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.query.has_columns || (self.query.offset == 0 && !self.query.more) {
            return None;
        }
        let size = Settings::global(cx).db_page_size.max(1);
        let (offset, shown, more) = (self.query.offset, self.query.shown, self.query.more);
        Some(
            h_flex()
                .gap_0p5()
                .child(
                    Button::new("db-first")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevrons-left"))
                        .tooltip(tr!("db-first-page"))
                        .disabled(offset == 0)
                        .on_click(
                            cx.listener(move |this, _, _window, cx| this.page_db_query(0, cx)),
                        ),
                )
                .child(
                    Button::new("db-previous")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevron-left"))
                        .tooltip(tr!("db-previous-page"))
                        .disabled(offset == 0)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(offset.saturating_sub(size), cx)
                        })),
                )
                .child(
                    Button::new("db-next")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevron-right"))
                        .tooltip(tr!("db-next-page"))
                        .disabled(!more)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(offset + shown, cx)
                        })),
                ),
        )
    }

    /// La taille de la fenêtre, qui est un réglage : on la choisit une fois
    /// pour toutes les consoles, pas à chaque requête.
    fn render_page_size(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = Settings::global(cx).db_page_size.max(1);
        Button::new("db-page-size")
            .ghost()
            .xsmall()
            .label(SharedString::from(current.to_string()))
            .tooltip(tr!("db-page-size"))
            .dropdown_menu(move |menu, _window, _cx| {
                PAGE_SIZES.iter().fold(menu, |menu, size| {
                    let size = *size;
                    menu.item(
                        PopupMenuItem::new(SharedString::from(size.to_string()))
                            .checked(size == current)
                            .on_click(move |_, _window, cx| {
                                Settings::update_global(cx, |settings| {
                                    settings.db_page_size = size;
                                });
                            }),
                    )
                })
            })
    }

    fn render_db_results(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let centered = |message: SharedString, error: bool, cx: &Context<Self>| {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(if error {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                })
                .child(message)
                .into_any_element()
        };
        if let Some(error) = self.query.error.clone() {
            // L'erreur du moteur, telle quelle : c'est elle qui dit la ligne
            // et la colonne fautives, et la reformuler n'apporterait que des
            // approximations.
            return div()
                .id("db-error")
                .size_full()
                .overflow_scroll()
                .p_3()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().danger)
                .child(error)
                .into_any_element();
        }
        if self.query.sent.is_none() {
            return centered(tr!("db-run-hint"), false, cx);
        }
        if !self.query.has_columns {
            return centered(
                tr!("db-affected-short", { n: self.query.affected.unwrap_or(0) }),
                false,
                cx,
            );
        }
        if self.query.shown == 0 {
            return centered(tr!("db-no-rows"), false, cx);
        }
        // Le lissage de la molette, comme partout ailleurs : la grille fait
        // couramment mille lignes, et un cran qui saute trois lignes d'un coup
        // fait perdre sa place à l'œil. La table peint ses propres barres,
        // d'où `smoothed` et non `scrolled`.
        let handle = self.db_table.read(cx).vertical_scroll_handle.clone();
        self.smoothed(
            "db-results",
            &handle,
            Axes::Vertical,
            window,
            DataTable::new(&self.db_table).stripe(true).bordered(false),
            cx,
        )
        .into_any_element()
    }
}

/// Complète les mots-clés SQL, les tables et les colonnes du schéma indexé.
///
/// **Le fournisseur filtre lui-même** : la liste de gpui-component affiche ce
/// qu'on lui rend, en surlignant le préfixe, sans rien écarter. Un schéma de
/// trois cents tables proposerait sinon trois cents lignes à la première
/// lettre tapée.
pub struct SqlCompletions {
    pub schema: Rc<RefCell<SchemaIndex>>,
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let source = text.to_string();
        let offset = offset.min(source.len());
        // Le mot en cours : ce qui précède le curseur et qui pourrait être un
        // identifiant. C'est lui que la complétion remplace.
        let start = source[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
            .last()
            .map(|(index, _)| index)
            .unwrap_or(offset);
        let prefix = source[start..offset].to_lowercase();

        // Un identifiant suivi d'un point restreint les candidats aux colonnes
        // de cette table : `users.` ne propose que ce qu'`users` contient.
        let qualifier = source[..start].strip_suffix('.').map(|before| {
            before
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
                .to_lowercase()
        });

        let mut items: Vec<(String, CompletionItemKind)> = Vec::new();
        {
            let schema = self.schema.borrow();
            match qualifier.filter(|name| !name.is_empty()) {
                Some(name) => {
                    if let Some((_, columns)) = schema
                        .tables
                        .iter()
                        .find(|(table, _)| table.to_lowercase() == name)
                    {
                        items.extend(
                            columns
                                .iter()
                                .map(|column| (column.clone(), CompletionItemKind::FIELD)),
                        );
                    }
                }
                None => {
                    let mut seen = std::collections::HashSet::new();
                    for (table, columns) in &schema.tables {
                        items.push((table.clone(), CompletionItemKind::CLASS));
                        for column in columns {
                            if seen.insert(column.clone()) {
                                items.push((column.clone(), CompletionItemKind::FIELD));
                            }
                        }
                    }
                    items.extend(
                        KEYWORDS
                            .iter()
                            .map(|word| (word.to_string(), CompletionItemKind::KEYWORD)),
                    );
                }
            }
        }

        // Le remplacement est donné explicitement : la plage de repli de
        // l'éditeur part du premier caractère du mot **déclencheur**, qui
        // englobe le `users.` d'une colonne qualifiée — on remplacerait la
        // table par sa colonne.
        let range = lsp_types::Range {
            start: text.offset_to_position(start),
            end: text.offset_to_position(offset),
        };
        let completions: Vec<CompletionItem> = items
            .into_iter()
            .filter(|(label, _)| prefix.is_empty() || label.to_lowercase().starts_with(&prefix))
            .take(50)
            .map(|(label, kind)| CompletionItem {
                filter_text: Some(label.clone()),
                kind: Some(kind),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: label.clone(),
                })),
                label,
                ..Default::default()
            })
            .collect();
        let _ = cx;
        Task::ready(Ok(CompletionResponse::Array(completions)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

/// L'état du partage éditeur / résultats, créé une fois avec la fenêtre.
pub fn split_state(cx: &mut App) -> gpui::Entity<ResizableState> {
    cx.new(|_| ResizableState::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un résultat de trois lignes, dont un nul et une valeur à virgule.
    fn results() -> Results {
        Results {
            rows: db::Rows {
                columns: vec!["id".into(), "email".into(), "name".into()],
                rows: vec![
                    vec![Some("1".into()), Some("a@x".into()), Some("Ada".into())],
                    vec![Some("2".into()), Some("b@x".into()), None],
                    vec![Some("3".into()), Some("c,d@x".into()), Some("Eve".into())],
                ],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Le rectangle se lit dans les deux sens : on tire aussi bien vers le
    /// haut et vers la gauche, et l'ancre reste celle du premier clic.
    #[test]
    fn a_selection_reads_the_same_from_either_corner() {
        let mut results = results();
        results.press(2, 2, false);
        assert!(results.drag_to(0, 1));
        assert!(!results.drag_to(0, 1), "rien n'a bougé, rien à repeindre");

        let selection = results.selection.unwrap();
        assert_eq!(selection.anchor, (2, 2), "l'ancre est le premier clic");
        assert!(selection.contains(1, 1) && selection.contains(0, 2));
        assert!(!selection.contains(0, 0), "la colonne 0 est hors du bloc");
        assert_eq!(selection.count(), 6);
    }

    /// Maj+clic déplace le curseur et garde l'ancre ; un clic nu recommence.
    #[test]
    fn a_shift_click_extends_and_a_plain_click_restarts() {
        let mut results = results();
        results.press(0, 0, false);
        results.press(2, 1, true);
        assert_eq!(results.selection.unwrap().anchor, (0, 0));
        assert_eq!(results.selection.unwrap().count(), 6);

        results.press(1, 1, false);
        assert_eq!(results.selection.unwrap().count(), 1);
    }

    /// Une cellule seule sort telle quelle : c'est une valeur qu'on va coller
    /// dans une requête, et l'encadrer serait une corvée à chaque collage.
    #[test]
    fn a_single_cell_is_copied_raw() {
        let mut results = results();
        results.press(2, 1, false);
        assert_eq!(results.selected_text(false).unwrap(), "c,d@x");

        // Avec les en-têtes, elle redevient un tableau — et la virgule ne se
        // fait plus encadrer, le presse-papiers séparant par des tabulations.
        assert_eq!(results.selected_text(true).unwrap(), "email\nc,d@x\n");
    }

    /// Un bloc sort en colonnes, et une valeur nulle sort vide plutôt que
    /// sous le mot « NULL » qui n'a de sens que dans la grille.
    #[test]
    fn a_block_is_copied_in_columns_and_null_is_empty() {
        let mut results = results();
        results.press(0, 1, false);
        results.drag_to(1, 2);
        assert_eq!(results.selected_text(false).unwrap(), "a@x\tAda\nb@x\t\n");

        results.select_all();
        assert_eq!(results.selection.unwrap().count(), 9);
        assert_eq!(results.all_text(), results.selected_text(true).unwrap());
    }

    /// Une ligne se copie avec ses noms de colonnes : c'est ce qu'on relit
    /// dans un message, où « 3, c,d@x, Eve » ne dirait rien.
    #[test]
    fn a_row_is_copied_under_its_headers() {
        let results = results();
        assert_eq!(
            results.row_text(2).unwrap(),
            "id\temail\tname\n3\tc,d@x\tEve\n"
        );
        assert!(results.row_text(9).is_none());
    }

    /// Un résultat vide n'a rien à sélectionner : `Ctrl+A` ne doit pas poser
    /// un rectangle sur zéro cellule, que la copie lirait hors des bornes.
    #[test]
    fn there_is_nothing_to_select_in_an_empty_result() {
        let mut results = Results::default();
        results.select_all();
        assert!(results.selection.is_none());
        assert!(results.selected_text(false).is_none());
    }
}
