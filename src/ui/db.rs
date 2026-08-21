//! Le panneau « Bases » : un explorateur de schémas.
//!
//! Quatre niveaux — connexion, base, table, colonne — dépliés **à la
//! demande** : un serveur de développement porte des dizaines de bases et
//! chacune des centaines de tables, et tout charger à l'ouverture du panneau
//! ferait payer à chaque démarrage ce dont on ne regarde qu'un coin. C'est
//! l'explorateur de PhpStorm, et le geste est le même : on déplie ce qu'on
//! cherche.
//!
//! Ce panneau **ne modifie jamais rien** : il lit des schémas. Ce qui écrit,
//! c'est la console SQL d'à côté (`ui::db_query`), et seulement dans les
//! limites que le compte de connexion autorise.
//!
//! Les connexions viennent des réglages, comme les profils d'agent : c'est le
//! deuxième niveau du système d'extension décrit dans CLAUDE.md — une
//! déclaration, pas du code.

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Sizable,
};

use crate::db;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// Largeur d'un niveau d'indentation, et du filet qui le marque. La même que
/// celle de l'explorateur de projet : ce sont deux arbres côte à côte.
const INDENT: f32 = 12.;

/// Ce qu'on sait d'une lecture : rien, elle est partie, elle est arrivée, elle
/// a échoué.
///
/// Quatre états et non un `Option<Result<…>>` : « pas encore demandé » et « en
/// route » se dessinent différemment — un nœud vide et une roue qui tourne —
/// et les confondre fait relancer la commande à chaque frame.
#[derive(Debug)]
pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// Une lecture qui n'a pas abouti se relance ; une lecture en route, non.
    fn needs_loading(&self) -> bool {
        matches!(self, Load::Idle | Load::Failed(_))
    }
}

pub struct ConnectionState {
    pub config: db::Connection,
    /// L'identité de la connexion, mot de passe exclu : c'est par elle que les
    /// réponses des workers retrouvent leur place.
    pub key: String,
    pub expanded: bool,
    pub databases: Load<Vec<DatabaseState>>,
}

pub struct DatabaseState {
    pub info: db::Database,
    pub expanded: bool,
    pub tables: Load<Vec<TableState>>,
}

pub struct TableState {
    pub info: db::Table,
    pub expanded: bool,
    pub columns: Load<Vec<db::Column>>,
}

/// Une ligne affichée.
///
/// Des **indices** et non des valeurs : la même colonne apparaît sous sa table
/// dépliée et dans le résultat d'un filtre, et un arbre de dix mille entrées
/// ferait sinon autant de clones de chaînes à chaque reconstruction. C'est la
/// raison qui vaut déjà pour `ui::tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Connection {
        connection: usize,
    },
    Database {
        connection: usize,
        database: usize,
    },
    Table {
        connection: usize,
        database: usize,
        table: usize,
    },
    Column {
        connection: usize,
        database: usize,
        table: usize,
        column: usize,
    },
    /// Une ligne qui dit ce qui se passe : un chargement en cours, une erreur.
    Status {
        depth: usize,
        loading: bool,
        message: SharedString,
    },
}

impl Entry {
    fn depth(&self) -> usize {
        match self {
            Entry::Connection { .. } => 0,
            Entry::Database { .. } => 1,
            Entry::Table { .. } => 2,
            Entry::Column { .. } => 3,
            Entry::Status { depth, .. } => *depth,
        }
    }
}

/// L'état du panneau.
#[derive(Default)]
pub struct DbState {
    pub connections: Vec<ConnectionState>,
    /// Les lignes affichées, reconstruites à chaque changement d'état, jamais
    /// au rendu : la fermeture d'`uniform_list` tourne pour chaque ligne
    /// visible à chaque frame.
    pub entries: Vec<Entry>,
    /// La requête de recherche pour laquelle `entries` a été bâtie.
    pub query: String,
    /// La ligne sous le curseur, par indice dans `entries`.
    ///
    /// Un indice et non une identité — contrairement à l'explorateur de
    /// projet, dont l'arbre se reconstruit sous le curseur à chaque frappe :
    /// ici la reconstruction est rare, et un indice suffit.
    pub cursor: Option<usize>,
    /// Les connexions qu'un « tout indexer » est en train de parcourir.
    pub indexing: HashSet<String>,
}

impl ClaudhubApp {
    /// Aligne la liste des connexions sur les réglages.
    ///
    /// Appelée au rendu, comme tout ce qui dépend d'un réglage. L'état d'une
    /// connexion est **repris par sa clé** : corriger un mot de passe ou
    /// renommer une connexion ne doit pas refermer l'arbre qu'on venait de
    /// déplier.
    pub(super) fn sync_db_connections(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<db::Connection> = Settings::global(cx)
            .databases
            .iter()
            .filter(|connection| connection.is_usable())
            .cloned()
            .collect();
        let same = wanted.len() == self.db.connections.len()
            && wanted
                .iter()
                .zip(self.db.connections.iter())
                .all(|(config, state)| config == &state.config);
        if same {
            return;
        }
        let mut previous = std::mem::take(&mut self.db.connections);
        self.db.connections = wanted
            .into_iter()
            .map(|config| {
                let key = config.key();
                match previous.iter().position(|state| state.key == key) {
                    Some(index) => {
                        let mut state = previous.remove(index);
                        state.config = config;
                        state
                    }
                    None => ConnectionState {
                        key,
                        config,
                        expanded: false,
                        databases: Load::Idle,
                    },
                }
            })
            .collect();
        // Les clés d'indexation survivraient à la connexion qu'elles visent.
        self.db
            .indexing
            .retain(|key| self.db.connections.iter().any(|state| &state.key == key));
        self.db_rebuild(cx);
    }

    fn connection_at(&self, index: usize) -> Option<&ConnectionState> {
        self.db.connections.get(index)
    }

    fn database_at(&self, connection: usize, database: usize) -> Option<&DatabaseState> {
        self.connection_at(connection)?
            .databases
            .ready()?
            .get(database)
    }

    fn table_at(&self, connection: usize, database: usize, table: usize) -> Option<&TableState> {
        self.database_at(connection, database)?
            .tables
            .ready()?
            .get(table)
    }

    fn database_mut(&mut self, connection: usize, database: usize) -> Option<&mut DatabaseState> {
        match &mut self.db.connections.get_mut(connection)?.databases {
            Load::Ready(databases) => databases.get_mut(database),
            _ => None,
        }
    }

    fn table_mut(
        &mut self,
        connection: usize,
        database: usize,
        table: usize,
    ) -> Option<&mut TableState> {
        match &mut self.database_mut(connection, database)?.tables {
            Load::Ready(tables) => tables.get_mut(table),
            _ => None,
        }
    }

    /// L'indice de la connexion qui porte cette clé.
    fn connection_by_key(&self, key: &str) -> Option<usize> {
        self.db
            .connections
            .iter()
            .position(|state| state.key == key)
    }

    // — Chargements ————————————————————————————————————————————————

    fn db_load_databases(&mut self, connection: usize, cx: &mut Context<Self>) {
        let Some(state) = self.db.connections.get_mut(connection) else {
            return;
        };
        state.databases = Load::Loading;
        let config = state.config.clone();
        self.git.send(Cmd::DbDatabases { connection: config });
        self.db_rebuild(cx);
    }

    fn db_load_tables(&mut self, connection: usize, database: usize, cx: &mut Context<Self>) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(state) = self.database_mut(connection, database) else {
            return;
        };
        let name = state.info.name.clone();
        state.tables = Load::Loading;
        self.git.send(Cmd::DbTables {
            connection: config,
            database: name,
        });
        self.db_rebuild(cx);
    }

    fn db_load_columns(
        &mut self,
        connection: usize,
        database: usize,
        table: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(database_name) = self
            .database_at(connection, database)
            .map(|d| d.info.name.clone())
        else {
            return;
        };
        let Some(state) = self.table_mut(connection, database, table) else {
            return;
        };
        let name = state.info.name.clone();
        state.columns = Load::Loading;
        self.git.send(Cmd::DbColumns {
            connection: config,
            database: database_name,
            table: name,
        });
        self.db_rebuild(cx);
    }

    /// Charge les colonnes de **toutes** les tables d'une base.
    ///
    /// Une commande et non une par table : c'est ce qui rend le filtre et les
    /// complétions abordables sur un schéma de trois cents tables.
    fn db_load_all_columns(&mut self, connection: usize, database: usize, cx: &mut Context<Self>) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(state) = self.database_mut(connection, database) else {
            return;
        };
        let name = state.info.name.clone();
        if let Load::Ready(tables) = &mut state.tables {
            for table in tables.iter_mut() {
                if table.columns.needs_loading() {
                    table.columns = Load::Loading;
                }
            }
        }
        self.git.send(Cmd::DbAllColumns {
            connection: config,
            database: name,
        });
        self.db_rebuild(cx);
    }

    // — Arrivées ———————————————————————————————————————————————————

    pub(super) fn db_databases_arrived(
        &mut self,
        key: String,
        databases: crate::runtime::protocol::DbResult<Vec<db::Database>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return; // the connection was removed while waiting
        };
        let Some(state) = self.db.connections.get_mut(index) else {
            return;
        };
        state.databases = match databases {
            Ok(list) => Load::Ready(
                list.into_iter()
                    .map(|info| DatabaseState {
                        info,
                        expanded: false,
                        tables: Load::Idle,
                    })
                    .collect(),
            ),
            Err(message) => Load::Failed(message),
        };
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_tables_arrived(
        &mut self,
        key: String,
        database: String,
        tables: crate::runtime::protocol::DbResult<Vec<db::Table>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        if let Some(state) = self.database_mut(index, position) {
            state.tables = match tables {
                Ok(list) => Load::Ready(
                    list.into_iter()
                        .map(|info| TableState {
                            info,
                            expanded: false,
                            columns: Load::Idle,
                        })
                        .collect(),
                ),
                Err(message) => Load::Failed(message),
            };
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_columns_arrived(
        &mut self,
        key: String,
        database: String,
        table: String,
        columns: crate::runtime::protocol::DbResult<Vec<db::Column>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        let Some(table_position) = self.table_position(index, position, &table) else {
            return;
        };
        if let Some(state) = self.table_mut(index, position, table_position) {
            state.columns = match columns {
                Ok(list) => Load::Ready(list),
                Err(message) => Load::Failed(message),
            };
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_all_columns_arrived(
        &mut self,
        key: String,
        database: String,
        columns: crate::runtime::protocol::DbResult<BTreeMap<String, Vec<db::Column>>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        // La console SQL complète sur ce que le panneau a indexé : elle
        // profite donc de la même lecture, sans en lancer une seconde.
        if let Ok(indexed) = &columns {
            self.db_schema_indexed(&key, &database, indexed);
        }
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        if let Some(state) = self.database_mut(index, position) {
            if let Load::Ready(tables) = &mut state.tables {
                match columns {
                    Ok(mut indexed) => {
                        for table in tables.iter_mut() {
                            if matches!(table.columns, Load::Loading) {
                                // Une table absente du lot a disparu entre la
                                // demande et la réponse : la marquer chargée
                                // évite de la redemander à chaque frame.
                                table.columns = Load::Ready(
                                    indexed.remove(&table.info.name).unwrap_or_default(),
                                );
                            }
                        }
                    }
                    Err(message) => {
                        for table in tables.iter_mut() {
                            if matches!(table.columns, Load::Loading) {
                                table.columns = Load::Failed(message.clone());
                            }
                        }
                    }
                }
            }
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    fn database_position(&self, connection: usize, name: &str) -> Option<usize> {
        self.connection_at(connection)?
            .databases
            .ready()?
            .iter()
            .position(|state| state.info.name == name)
    }

    fn table_position(&self, connection: usize, database: usize, name: &str) -> Option<usize> {
        self.database_at(connection, database)?
            .tables
            .ready()?
            .iter()
            .position(|state| state.info.name == name)
    }

    // — L'arbre ————————————————————————————————————————————————————

    /// Reconstruit les lignes affichées.
    pub(super) fn db_rebuild(&mut self, cx: &mut Context<Self>) {
        // La ligne sous le curseur est suivie par sa valeur, pas par son
        // indice : un dépliage insère des lignes au-dessus d'elle.
        let previous = self
            .db
            .cursor
            .and_then(|index| self.db.entries.get(index).cloned());
        let query = self.db.query.clone();
        self.db.entries = if query.trim().is_empty() {
            self.db_expanded_entries()
        } else {
            self.db_filtered_entries(&query)
        };
        self.db.cursor = previous
            .and_then(|entry| self.db.entries.iter().position(|other| *other == entry))
            .or_else(|| {
                self.db
                    .cursor
                    .map(|index| index.min(self.db.entries.len().saturating_sub(1)))
                    .filter(|_| !self.db.entries.is_empty())
            });
        cx.notify();
    }

    fn db_expanded_entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        for (connection, state) in self.db.connections.iter().enumerate() {
            entries.push(Entry::Connection { connection });
            if !state.expanded {
                continue;
            }
            match &state.databases {
                Load::Idle => {}
                Load::Loading => entries.push(status(1, true, tr!("db-connecting"))),
                Load::Failed(message) => entries.push(status(1, false, message.clone().into())),
                Load::Ready(databases) => {
                    for (database, state) in databases.iter().enumerate() {
                        entries.push(Entry::Database {
                            connection,
                            database,
                        });
                        if !state.expanded {
                            continue;
                        }
                        match &state.tables {
                            Load::Idle => {}
                            Load::Loading => {
                                entries.push(status(2, true, tr!("db-loading-tables")))
                            }
                            Load::Failed(message) => {
                                entries.push(status(2, false, message.clone().into()))
                            }
                            Load::Ready(tables) => {
                                for (table, state) in tables.iter().enumerate() {
                                    entries.push(Entry::Table {
                                        connection,
                                        database,
                                        table,
                                    });
                                    if !state.expanded {
                                        continue;
                                    }
                                    match &state.columns {
                                        Load::Idle => {}
                                        Load::Loading => {
                                            entries.push(status(3, true, tr!("db-loading-columns")))
                                        }
                                        Load::Failed(message) => {
                                            entries.push(status(3, false, message.clone().into()))
                                        }
                                        Load::Ready(columns) => {
                                            for column in 0..columns.len() {
                                                entries.push(Entry::Column {
                                                    connection,
                                                    database,
                                                    table,
                                                    column,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        entries
    }

    /// L'arbre filtré : les tables et les colonnes dont le nom correspond, avec
    /// leurs ancêtres, **sans tenir compte des replis**.
    ///
    /// Un résultat caché dans un nœud replié ne se verrait pas, et la
    /// recherche paraîtrait n'avoir rien trouvé — c'est la règle que suivent
    /// déjà l'explorateur et la liste de revue.
    fn db_filtered_entries(&self, query: &str) -> Vec<Entry> {
        let hit = |name: &str| crate::ui::find::matches(query, name);
        let mut entries = Vec::new();
        for (connection, state) in self.db.connections.iter().enumerate() {
            let mut children = Vec::new();
            match &state.databases {
                Load::Loading => children.push(status(1, true, tr!("db-connecting"))),
                Load::Ready(databases) => {
                    for (database, state) in databases.iter().enumerate() {
                        let mut rows = Vec::new();
                        match &state.tables {
                            Load::Loading => rows.push(status(2, true, tr!("db-loading-tables"))),
                            Load::Ready(tables) => {
                                let mut indexing = false;
                                for (table, state) in tables.iter().enumerate() {
                                    let mut columns = Vec::new();
                                    match &state.columns {
                                        Load::Ready(list) => {
                                            for (column, info) in list.iter().enumerate() {
                                                if hit(&info.name) {
                                                    columns.push(Entry::Column {
                                                        connection,
                                                        database,
                                                        table,
                                                        column,
                                                    });
                                                }
                                            }
                                        }
                                        Load::Loading => indexing = true,
                                        _ => {}
                                    }
                                    if hit(&state.info.name) || !columns.is_empty() {
                                        rows.push(Entry::Table {
                                            connection,
                                            database,
                                            table,
                                        });
                                        rows.append(&mut columns);
                                    }
                                }
                                if indexing {
                                    rows.push(status(2, true, tr!("db-indexing")));
                                }
                            }
                            _ => {}
                        }
                        if !rows.is_empty() || hit(&state.info.name) {
                            children.push(Entry::Database {
                                connection,
                                database,
                            });
                            children.append(&mut rows);
                        }
                    }
                }
                _ => {}
            }
            if !children.is_empty() || hit(&state.config.label()) {
                entries.push(Entry::Connection { connection });
                entries.append(&mut children);
            }
        }
        entries
    }

    /// Lance ce qu'il faut pour que le filtre voie l'arbre entier.
    ///
    /// **Les connexions qu'on n'a jamais dépliées sont laissées tranquilles** :
    /// taper trois lettres dans un champ de recherche ne doit pas ouvrir une
    /// connexion vers un serveur de production. Ce que ce parcours complète,
    /// c'est ce qui est déjà ouvert ; « tout indexer » est le geste qui se
    /// connecte partout, et il est explicite.
    fn db_index_for_filter(&mut self, cx: &mut Context<Self>) {
        for connection in 0..self.db.connections.len() {
            let Some(state) = self.connection_at(connection) else {
                continue;
            };
            let Some(databases) = state.databases.ready() else {
                continue;
            };
            for database in 0..databases.len() {
                let Some(state) = self.database_at(connection, database) else {
                    break;
                };
                match &state.tables {
                    Load::Idle => self.db_load_tables(connection, database, cx),
                    Load::Ready(tables)
                        if tables.iter().any(|table| table.columns.needs_loading()) =>
                    {
                        self.db_load_all_columns(connection, database, cx)
                    }
                    _ => {}
                }
            }
        }
    }

    /// Indexe tout, y compris ce qui n'a jamais été déplié.
    ///
    /// À la différence de l'indexation implicite du filtre, celui-ci **se
    /// connecte partout, exprès** : c'est ce qu'on demande quand on veut que
    /// la recherche et les complétions couvrent le schéma entier.
    pub(super) fn db_index_all(&mut self, cx: &mut Context<Self>) {
        for connection in 0..self.db.connections.len() {
            let Some(state) = self.db.connections.get(connection) else {
                continue;
            };
            self.db.indexing.insert(state.key.clone());
            // Une demande explicite retente ce qui avait échoué ; la
            // continuation, elle, ne retente jamais — ce serait une boucle.
            if state.databases.needs_loading() {
                self.db_load_databases(connection, cx);
            } else if state.databases.ready().is_some() {
                self.db_continue_indexing(connection, cx);
            }
        }
        cx.notify();
    }

    /// Fait avancer l'indexation d'une connexion à chaque lecture qui arrive,
    /// jusqu'à ce qu'il ne reste rien à demander.
    fn db_continue_indexing(&mut self, connection: usize, cx: &mut Context<Self>) {
        let Some(key) = self.connection_at(connection).map(|s| s.key.clone()) else {
            return;
        };
        if !self.db.indexing.contains(&key) {
            return;
        }
        let mut pending = false;
        match self.connection_at(connection).map(|s| &s.databases) {
            Some(Load::Loading) => pending = true,
            Some(Load::Ready(databases)) => {
                for database in 0..databases.len() {
                    let Some(state) = self.database_at(connection, database) else {
                        break;
                    };
                    match &state.tables {
                        Load::Idle => {
                            pending = true;
                            self.db_load_tables(connection, database, cx);
                        }
                        Load::Loading => pending = true,
                        Load::Failed(_) => {}
                        Load::Ready(tables) => {
                            if tables.iter().any(|table| table.columns.needs_loading()) {
                                pending = true;
                                self.db_load_all_columns(connection, database, cx);
                            } else if tables
                                .iter()
                                .any(|table| matches!(table.columns, Load::Loading))
                            {
                                pending = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if !pending {
            self.db.indexing.remove(&key);
        }
    }

    /// Oublie ce qu'on savait d'une connexion, et le relit si elle est ouverte.
    pub(super) fn db_refresh(&mut self, connection: Option<usize>, cx: &mut Context<Self>) {
        let targets: Vec<usize> = match connection {
            Some(index) => vec![index],
            None => (0..self.db.connections.len()).collect(),
        };
        for index in targets {
            let Some(state) = self.db.connections.get_mut(index) else {
                continue;
            };
            state.databases = Load::Idle;
            if state.expanded {
                self.db_load_databases(index, cx);
            }
        }
        self.db_rebuild(cx);
    }

    /// Déplie ou replie la ligne, et lance la lecture qui manque.
    pub(super) fn db_toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        self.db.cursor = Some(index);
        match entry {
            Entry::Connection { connection } => {
                let Some(state) = self.db.connections.get_mut(connection) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.databases.needs_loading();
                if load {
                    self.db_load_databases(connection, cx);
                }
            }
            Entry::Database {
                connection,
                database,
            } => {
                let Some(state) = self.database_mut(connection, database) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.tables.needs_loading();
                if load {
                    self.db_load_tables(connection, database, cx);
                }
            }
            Entry::Table {
                connection,
                database,
                table,
            } => {
                let Some(state) = self.table_mut(connection, database, table) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.columns.needs_loading();
                if load {
                    self.db_load_columns(connection, database, table, cx);
                }
            }
            Entry::Column { .. } | Entry::Status { .. } => {}
        }
        self.db_rebuild(cx);
    }

    fn db_expanded(&self, entry: &Entry) -> Option<bool> {
        match *entry {
            Entry::Connection { connection } => Some(self.connection_at(connection)?.expanded),
            Entry::Database {
                connection,
                database,
            } => Some(self.database_at(connection, database)?.expanded),
            Entry::Table {
                connection,
                database,
                table,
            } => Some(self.table_at(connection, database, table)?.expanded),
            Entry::Column { .. } | Entry::Status { .. } => None,
        }
    }

    // — Le clavier —————————————————————————————————————————————————

    pub(super) fn db_step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.db.entries.is_empty() {
            return;
        }
        let last = self.db.entries.len() - 1;
        let next = match self.db.cursor {
            Some(index) => (index as isize + delta).clamp(0, last as isize) as usize,
            None => 0,
        };
        self.db.cursor = Some(next);
        self.db_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// Droite : déplier, ou descendre d'une ligne. Gauche : replier, ou
    /// remonter au parent. Ce sont les gestes de tout explorateur.
    pub(super) fn db_fold_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some(index) = self.db.cursor else { return };
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        match (open, self.db_expanded(&entry)) {
            (true, Some(false)) | (false, Some(true)) => self.db_toggle(index, cx),
            (true, _) => self.db_step_cursor(1, cx),
            (false, _) => {
                let depth = entry.depth();
                if let Some(parent) = self.db.entries[..index]
                    .iter()
                    .rposition(|other| other.depth() < depth)
                {
                    self.db.cursor = Some(parent);
                    self.db_scroll
                        .scroll_to_item(parent, gpui::ScrollStrategy::Center);
                    cx.notify();
                }
            }
        }
    }

    /// Entrée : ouvrir une console sur la ligne, comme un double-clic.
    pub(super) fn db_open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.db.cursor else { return };
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        match entry {
            Entry::Connection { .. } | Entry::Database { .. } => self.db_toggle(index, cx),
            Entry::Table { .. } | Entry::Column { .. } => self.open_db_console(&entry, window, cx),
            Entry::Status { .. } => {}
        }
    }

    /// Ouvre la console SQL sur ce que désigne une ligne.
    ///
    /// Une table donne un `SELECT * FROM …` tout prêt : c'est la première
    /// chose qu'on écrit après avoir trouvé une table, et le taper à chaque
    /// fois est ce qui fait qu'on ne se sert pas d'un explorateur.
    pub(super) fn open_db_console(
        &mut self,
        entry: &Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connection, database, table) = match *entry {
            Entry::Connection { connection } => (connection, None, None),
            Entry::Database {
                connection,
                database,
            } => (
                connection,
                self.database_at(connection, database)
                    .map(|state| state.info.name.clone()),
                None,
            ),
            Entry::Table {
                connection,
                database,
                table,
            }
            | Entry::Column {
                connection,
                database,
                table,
                ..
            } => (
                connection,
                self.database_at(connection, database)
                    .map(|state| state.info.name.clone()),
                self.table_at(connection, database, table)
                    .map(|state| state.info.name.clone()),
            ),
            Entry::Status { .. } => return,
        };
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        self.start_db_console(config, database, table, window, cx);
    }

    // — Le rendu ———————————————————————————————————————————————————

    pub(super) fn render_db(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_db_connections(cx);
        let query = self.query(Pane::Db, cx);
        if self.db.query != query {
            self.db.query = query;
            if !self.db.query.trim().is_empty() {
                self.db_index_for_filter(cx);
            }
            self.db_rebuild(cx);
        }
        let find = self.render_find(Pane::Db, cx);
        let bar = self.render_db_bar(cx);
        let vim = Settings::global(cx).vim_mode;
        let focus = self.db_focus.clone();
        let scroll = self.db_scroll.clone();

        if self.db.connections.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(cx.theme().muted_foreground)
                        .child(icon("database"))
                        .child(div().text_sm().px_4().child(tr!("db-empty")))
                        .child(
                            Button::new("db-add-first")
                                .outline()
                                .small()
                                .icon(icon("plus"))
                                .label(tr!("db-add-connection"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_settings_at(
                                        crate::ui::settings_view::Page::Databases,
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                )
                .into_any_element();
        }

        let entries = Rc::new(self.db.entries.clone());
        let look = Look::of(cx);
        let cursor = self.db.cursor;
        let entity = cx.entity();
        let count = entries.len();

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div()
                    .id("db-tree")
                    // Les flèches appartiennent à cet arbre quand il a le
                    // focus, comme celles de l'explorateur au sien.
                    .key_context(crate::ui::shortcuts::db_context(vim))
                    .track_focus(&focus)
                    .flex_1()
                    .min_h_0()
                    .child(
                        self.scrolled(
                            "db-tree-bar",
                            &scroll,
                            crate::ui::motion::Axes::Vertical,
                            window,
                            uniform_list("db-entries", count, move |visible, _window, cx| {
                                visible
                                    .map(|index| {
                                        render_row(&entries, index, cursor, &look, &entity, cx)
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .size_full()
                            // Le retrait appartient à la liste : une marge posée
                            // sur une entrée d'`uniform_list` est ignorée, la
                            // liste calculant elle-même la taille de ses items.
                            .px_1()
                            .track_scroll(&scroll.clone()),
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_db_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let indexing = !self.db.indexing.is_empty();
        let count = self.db.connections.len();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("database").xsmall())
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("db-connections-count", { n: count })),
            )
            .child(
                Button::new("db-index-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("zap"))
                    .tooltip(tr!("db-index-all"))
                    .disabled(indexing)
                    .on_click(cx.listener(|this, _, _window, cx| this.db_index_all(cx))),
            )
            .child(
                Button::new("db-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .on_click(cx.listener(|this, _, _window, cx| this.db_refresh(None, cx))),
            )
            .child(
                Button::new("db-add")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("db-add-connection"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings_at(crate::ui::settings_view::Page::Databases, window, cx)
                    })),
            )
    }
}

fn status(depth: usize, loading: bool, message: SharedString) -> Entry {
    Entry::Status {
        depth,
        loading,
        message,
    }
}

/// Ce que le thème donne à une ligne, lu une fois par frame et non par ligne.
#[derive(Clone)]
struct Look {
    height: gpui::Pixels,
    radius: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    guide: gpui::Hsla,
    danger: gpui::Hsla,
    warning: gpui::Hsla,
    info: gpui::Hsla,
    success: gpui::Hsla,
}

impl Look {
    fn of(cx: &gpui::App) -> Self {
        Self {
            height: crate::ui::theme::row_height(cx),
            radius: cx.theme().radius,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            guide: cx.theme().border.opacity(0.7),
            danger: cx.theme().danger,
            warning: cx.theme().warning,
            info: cx.theme().info,
            success: cx.theme().success,
        }
    }
}

fn indent_guides(depth: usize, look: &Look) -> impl IntoIterator<Item = gpui::Div> + use<> {
    let guide = look.guide;
    (0..depth).map(move |_| {
        div()
            .w(px(INDENT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(guide)
    })
}

/// Une ligne de l'arbre.
///
/// L'état est relu ici, dans la fermeture de la liste, et non recopié dans
/// `Entry` : une entrée ne porte que des indices, et le nom d'une table n'a
/// pas à être cloné à chaque reconstruction pour être affiché quelques frames.
fn render_row(
    entries: &Rc<Vec<Entry>>,
    index: usize,
    cursor: Option<usize>,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(entry) = entries.get(index).cloned() else {
        return div().into_any_element();
    };
    let app = entity.read(cx);
    let at_cursor = cursor == Some(index);
    let depth = entry.depth();

    if let Entry::Status {
        loading, message, ..
    } = &entry
    {
        return h_flex()
            .id(("db-status", index))
            .h(look.height)
            .items_center()
            .pl_1()
            .pr_2()
            .children(indent_guides(depth, look))
            .child(div().w(px(14.)).flex_none())
            .child(
                icon(if *loading {
                    "loader-circle"
                } else {
                    "circle-x"
                })
                .xsmall()
                .text_color(if *loading { look.muted } else { look.danger }),
            )
            .child(
                div()
                    .pl_1()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(if *loading { look.muted } else { look.danger })
                    .child(message.clone()),
            )
            .into_any_element();
    }

    let Some((glyph, tint, name, detail, tooltip)) = describe(app, &entry, look) else {
        return div().into_any_element();
    };
    let expanded = app.db_expanded(&entry);
    let for_menu = entry.clone();
    let (click, menu) = (entity.clone(), entity.clone());

    h_flex()
        .id(("db-row", index))
        .h(look.height)
        .rounded(look.radius)
        .pl_1()
        .pr_2()
        .items_center()
        .cursor_pointer()
        .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, window, cx| {
            click.update(cx, |this, cx| {
                // Le clic reprend le focus : sans cela, les flèches
                // continueraient de parcourir l'explorateur de projet.
                this.db_focus.clone().focus(window, cx);
                this.db_toggle(index, cx);
            });
        })
        .children(indent_guides(depth, look))
        .child(match expanded {
            Some(true) => icon("chevron-down")
                .xsmall()
                .text_color(look.muted)
                .into_any_element(),
            Some(false) => icon("chevron-right")
                .xsmall()
                .text_color(look.muted)
                .into_any_element(),
            // La place du chevron qu'une colonne n'a pas : sans elle, les noms
            // ne s'alignent pas d'un niveau à l'autre.
            None => div().w(px(14.)).flex_none().into_any_element(),
        })
        .child(icon(glyph).xsmall().text_color(tint))
        .child(div().pl_1().truncate().text_sm().child(name))
        .when_some(detail, |el, detail| {
            el.child(
                div()
                    .pl_1()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(look.muted)
                    .child(detail),
            )
        })
        .when_some(tooltip, |el, tooltip| {
            el.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
        })
        .context_menu(move |popup, _window, _cx| row_menu(popup, &menu, &for_menu))
        .into_any_element()
}

/// L'icône, la teinte, le nom, le détail et l'infobulle d'une ligne.
///
/// Une seule fonction pour les quatre niveaux : ce sont les mêmes quatre
/// choses, et les séparer ferait quatre fois la même mise en page.
#[allow(clippy::type_complexity)]
fn describe(
    app: &ClaudhubApp,
    entry: &Entry,
    look: &Look,
) -> Option<(
    &'static str,
    gpui::Hsla,
    SharedString,
    Option<SharedString>,
    Option<SharedString>,
)> {
    match *entry {
        Entry::Connection { connection } => {
            let state = app.connection_at(connection)?;
            let tint = match &state.databases {
                Load::Ready(_) => look.success,
                Load::Failed(_) => look.danger,
                _ => look.muted,
            };
            let glyph = match state.config.engine {
                db::Engine::Sqlite => "database",
                db::Engine::Mysql => "globe",
            };
            Some((
                glyph,
                tint,
                state.config.label().into(),
                Some(state.config.detail().into()),
                Some(
                    format!(
                        "{} · {}",
                        state.config.engine.label(),
                        state.config.detail()
                    )
                    .into(),
                ),
            ))
        }
        Entry::Database {
            connection,
            database,
        } => {
            let state = app.database_at(connection, database)?;
            let mut parts = Vec::new();
            parts.extend(state.info.charset.clone());
            parts.extend(state.info.collation.clone());
            Some((
                "database",
                look.muted,
                state.info.name.clone().into(),
                None,
                (!parts.is_empty()).then(|| parts.join(" · ").into()),
            ))
        }
        Entry::Table {
            connection,
            database,
            table,
        } => {
            let state = app.table_at(connection, database, table)?;
            let info = &state.info;
            let mut parts = Vec::new();
            if info.view {
                parts.push(tr!("db-view").to_string());
            }
            parts.extend(info.engine.clone());
            if let Some(rows) = info.rows {
                parts.push(format!("~{} {}", db::count(rows), tr!("db-rows")));
            }
            parts.extend(info.bytes.map(db::size));
            parts.extend(info.collation.clone());
            parts.extend(info.comment.clone());
            Some((
                if info.view { "eye" } else { "table" },
                look.muted,
                info.name.clone().into(),
                info.rows.map(|rows| {
                    SharedString::from(format!("{} {}", db::count(rows), tr!("db-rows")))
                }),
                (!parts.is_empty()).then(|| parts.join(" · ").into()),
            ))
        }
        Entry::Column {
            connection,
            database,
            table,
            column,
        } => {
            let state = app.table_at(connection, database, table)?;
            let info = state.columns.ready()?.get(column)?;
            // La clé primaire et la clé étrangère portent le même glyphe et
            // deux teintes : c'est la même famille — ce par quoi une ligne se
            // désigne —, et deux dessins différents ne diraient rien de plus.
            let (glyph, tint) = if info.primary_key {
                ("tag", look.warning)
            } else if info.foreign_key.is_some() {
                ("tag", look.info)
            } else {
                ("columns-2", look.muted)
            };
            let mut parts = vec![info.data_type.clone()];
            parts.push(
                if info.nullable {
                    tr!("db-nullable")
                } else {
                    tr!("db-not-null")
                }
                .to_string(),
            );
            if let Some(default) = &info.default {
                parts.push(format!("{} {default}", tr!("db-default")));
            }
            if info.primary_key {
                parts.push(tr!("db-primary-key").to_string());
            }
            if info.unique {
                parts.push(tr!("db-unique").to_string());
            }
            if info.auto_increment {
                parts.push(tr!("db-auto-increment").to_string());
            }
            if let Some(target) = &info.foreign_key {
                parts.push(format!("{} {target}", tr!("db-references")));
            }
            parts.extend(info.charset.clone());
            parts.extend(info.collation.clone());
            parts.extend(info.comment.clone());
            Some((
                glyph,
                tint,
                info.name.clone().into(),
                Some(info.data_type.clone().into()),
                Some(parts.join(" · ").into()),
            ))
        }
        Entry::Status { .. } => None,
    }
}

/// Le menu d'une ligne : interroger, rafraîchir, copier, retirer.
fn row_menu(popup: PopupMenu, entity: &Entity<ClaudhubApp>, entry: &Entry) -> PopupMenu {
    let is_table = matches!(entry, Entry::Table { .. } | Entry::Column { .. });
    let is_connection = matches!(entry, Entry::Connection { .. });
    let (console, refresh, copy, remove) = (
        entity.clone(),
        entity.clone(),
        entity.clone(),
        entity.clone(),
    );
    let (e1, e2, e3, e4) = (entry.clone(), entry.clone(), entry.clone(), entry.clone());
    popup
        .item(
            PopupMenuItem::new(if is_table {
                tr!("db-query-table")
            } else {
                tr!("db-new-console")
            })
            .icon(icon("play"))
            .on_click(move |_, window, cx| {
                console.update(cx, |this, cx| this.open_db_console(&e1, window, cx));
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("action-refresh"))
                .icon(icon("refresh-cw"))
                .on_click(move |_, _window, cx| {
                    refresh.update(cx, |this, cx| this.db_refresh_entry(&e2, cx));
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-name"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    copy.update(cx, |this, cx| this.db_copy_name(&e3, cx));
                }),
        )
        .when(is_connection, |popup| {
            popup.separator().item(
                PopupMenuItem::new(tr!("db-remove-connection"))
                    .icon(icon("trash-2"))
                    .on_click(move |_, _window, cx| {
                        remove.update(cx, |this, cx| this.db_remove_connection(&e4, cx));
                    }),
            )
        })
}

impl ClaudhubApp {
    /// Rafraîchit ce que désigne une ligne, et rien de plus : rouvrir tout un
    /// serveur pour relire une table serait une commande par base.
    pub(super) fn db_refresh_entry(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        match *entry {
            Entry::Connection { connection } => self.db_refresh(Some(connection), cx),
            Entry::Database {
                connection,
                database,
            } => {
                if let Some(state) = self.database_mut(connection, database) {
                    state.tables = Load::Idle;
                    let expanded = state.expanded;
                    if expanded {
                        self.db_load_tables(connection, database, cx);
                    }
                }
                self.db_rebuild(cx);
            }
            Entry::Table {
                connection,
                database,
                table,
            }
            | Entry::Column {
                connection,
                database,
                table,
                ..
            } => {
                if let Some(state) = self.table_mut(connection, database, table) {
                    state.columns = Load::Idle;
                    let expanded = state.expanded;
                    if expanded {
                        self.db_load_columns(connection, database, table, cx);
                    }
                }
                self.db_rebuild(cx);
            }
            Entry::Status { .. } => {}
        }
    }

    fn db_copy_name(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let Some(name) = self.db_entry_name(entry) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(name));
    }

    fn db_entry_name(&self, entry: &Entry) -> Option<String> {
        Some(match *entry {
            Entry::Connection { connection } => self.connection_at(connection)?.config.label(),
            Entry::Database {
                connection,
                database,
            } => self.database_at(connection, database)?.info.name.clone(),
            Entry::Table {
                connection,
                database,
                table,
            } => self
                .table_at(connection, database, table)?
                .info
                .name
                .clone(),
            Entry::Column {
                connection,
                database,
                table,
                column,
            } => self
                .table_at(connection, database, table)?
                .columns
                .ready()?
                .get(column)?
                .name
                .clone(),
            Entry::Status { .. } => return None,
        })
    }

    /// Retire une connexion des réglages.
    ///
    /// Par sa **valeur** et non par son indice : les réglages ont pu être
    /// réécrits depuis que le menu s'est ouvert, et un indice périmé
    /// supprimerait la voisine.
    fn db_remove_connection(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let Entry::Connection { connection } = *entry else {
            return;
        };
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        Settings::update_global(cx, |settings| {
            settings.databases.retain(|other| other != &config);
        });
        self.sync_db_connections(cx);
    }
}
