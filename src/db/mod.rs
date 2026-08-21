//! La couche base de données.
//!
//! Elle est à `src/ui/db*.rs` ce que `src/git/` est à la revue : des types et
//! des fonctions qui parlent au monde extérieur, sans une ligne de gpui, et
//! qui se testent sans fenêtre.
//!
//! **Un seul pilote, `sqlx`**, pour les deux moteurs — et pour le troisième
//! qu'on ajouterait. Il est asynchrone de bout en bout, d'où l'exécuteur
//! partagé de `runtime::executor` : le worker qui traite une commande fait un
//! `block_on` et attend exactement comme il attendait `git`. Ce que cela
//! achète et qu'un pilote bloquant ne pouvait pas donner : un délai qui
//! **annule vraiment** — on laisse tomber le futur, et le pilote ferme la
//! connexion en cours de route.
//!
//! **Une connexion par requête, jamais gardée.** Un panneau qui garde une
//! connexion ouverte sur un serveur qu'on n'interroge plus tient un descripteur
//! et un processus côté serveur pour rien, et il découvre la coupure du réseau
//! au pire moment. Le coût d'un `connect` est de quelques millisecondes en
//! local, et ces commandes vivent de toute façon dans la file du réseau.
//!
//! **La lecture seule est celle de SQLite.** Le fichier est ouvert avec
//! `SQLITE_OPEN_READONLY` : un `UPDATE` lancé par erreur dans la console SQL y
//! échoue, et c'est ce qu'on veut d'un explorateur. Pour MySQL, c'est le
//! serveur qui décide — les droits du compte de connexion sont la seule
//! barrière qui vaille, et en poser une seconde ici empêcherait un `UPDATE`
//! que l'utilisateur a le droit de faire.

pub mod mysql;
pub mod sqlite;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// Port de MySQL et de MariaDB quand la connexion n'en dit pas.
pub const DEFAULT_MYSQL_PORT: u16 = 3306;

/// Au-delà, la connexion est réputée impossible.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Au-delà, la requête est abandonnée.
///
/// Vraiment abandonnée : `timeout` laisse tomber le futur, et le pilote
/// ferme la connexion en cours de route. C'est ce qu'un pilote bloquant ne
/// sait pas faire — il faut lui demander de s'interrompre par un moyen qui lui
/// est propre, et ce qu'il n'a pas prévu ne s'interrompt pas du tout.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Enveloppe une lecture du délai commun.
///
/// À l'entrée de chaque fonction publique et non autour de chaque requête :
/// une introspection en fait plusieurs à la suite, et c'est le geste entier
/// qu'on abandonne, pas la troisième de ses requêtes.
async fn with_timeout<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(QUERY_TIMEOUT, future)
        .await
        .map_err(|_| anyhow::anyhow!("la base n'a pas répondu en {} s", QUERY_TIMEOUT.as_secs()))?
}

/// Le moteur d'une connexion.
///
/// Deux, et le troisième se lit dans ce type : ajouter PostgreSQL, c'est une
/// variante ici et un module à côté de `sqlite` et `mysql`, sans rien changer
/// ni au protocole ni aux vues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Sqlite,
    /// MySQL et MariaDB, qui parlent le même protocole et déclarent leur
    /// schéma dans le même `information_schema`.
    Mysql,
}

impl Engine {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Mysql => "mysql",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "mysql" | "mariadb" => Self::Mysql,
            _ => Self::Sqlite,
        }
    }

    /// Ce que le formulaire propose, dans l'ordre.
    pub const ALL: [Engine; 2] = [Engine::Sqlite, Engine::Mysql];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL / MariaDB",
        }
    }
}

/// Une connexion telle que les réglages la portent.
///
/// **Une structure plate et non une énumération à charge utile**, alors que
/// les deux moteurs n'ont presque aucun champ en commun : c'est ce qui rend le
/// formulaire des réglages possible — une ligne, des champs qu'on montre ou
/// qu'on cache selon le moteur — et ce qui fait qu'un fichier écrit par une
/// version antérieure se relit toujours, `#[serde(default)]` remplissant ce
/// qui manque. C'est le choix de `DatabaseConnectionContent` côté Zed, et pour
/// les mêmes raisons.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Connection {
    /// Ce que le panneau affiche. Vide, il est déduit du fichier ou du
    /// `user@host`.
    pub name: String,
    pub engine: Engine,
    /// SQLite : le chemin du fichier, `~/` développé à l'ouverture.
    pub path: String,
    pub host: String,
    /// `0` vaut « celui du moteur » : un port par défaut écrit en dur dans le
    /// fichier de réglages vieillirait mal, et zéro n'est pas un port.
    pub port: u16,
    pub user: String,
    /// En clair dans le fichier de réglages, qui est en 0600 sans être un
    /// coffre pour autant : préférez un compte en lecture seule.
    pub password: String,
    /// Les bases à montrer. Vide : toutes sauf celles du système.
    pub databases: Vec<String>,
}

/// Le mot de passe ne s'écrit jamais.
///
/// C'est ce qui permet à une `Cmd` de porter la connexion entière plutôt que
/// de la faire relire au worker : le protocole est journalisé — `log::warn!`
/// sur un échec, `{cmd:?}` sous un débogueur — et un secret qui traverse un
/// `Debug` dérivé finit dans un fichier de trace.
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("name", &self.name)
            .field("engine", &self.engine)
            .field("path", &self.path)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "…" })
            .field("databases", &self.databases)
            .finish()
    }
}

impl Connection {
    /// Le nom affiché : le sien, sinon celui qui se déduit de l'adresse.
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        match self.engine {
            Engine::Sqlite => std::path::Path::new(&self.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.clone()),
            Engine::Mysql => format!("{}@{}", self.user(), self.host()),
        }
    }

    /// L'adresse, telle que la ligne du panneau la montre en second.
    pub fn detail(&self) -> String {
        match self.engine {
            Engine::Sqlite => self.path.clone(),
            Engine::Mysql => format!("{}:{}", self.host(), self.port()),
        }
    }

    pub fn host(&self) -> String {
        let host = self.host.trim();
        if host.is_empty() {
            "localhost".to_string()
        } else {
            host.to_string()
        }
    }

    pub fn port(&self) -> u16 {
        if self.port == 0 {
            DEFAULT_MYSQL_PORT
        } else {
            self.port
        }
    }

    pub fn user(&self) -> String {
        let user = self.user.trim();
        if user.is_empty() {
            "root".to_string()
        } else {
            user.to_string()
        }
    }

    /// Une connexion sans adresse n'est pas une connexion : elle n'apparaît
    /// pas dans le panneau plutôt que d'y échouer à chaque ouverture.
    pub fn is_usable(&self) -> bool {
        match self.engine {
            Engine::Sqlite => !self.path.trim().is_empty(),
            Engine::Mysql => true,
        }
    }

    /// De quoi reconnaître une connexion d'un rendu à l'autre.
    ///
    /// **Le mot de passe n'y est pas** : c'est un secret, il ne change pas le
    /// schéma, et cette clé sert à retrouver l'état d'une connexion quand les
    /// réglages viennent d'être réécrits.
    pub fn key(&self) -> String {
        match self.engine {
            Engine::Sqlite => format!("sqlite:{}", self.path),
            Engine::Mysql => format!(
                "mysql:{}@{}:{}/{}",
                self.user(),
                self.host(),
                self.port(),
                self.databases.join(",")
            ),
        }
    }
}

/// Une base de données d'une connexion.
///
/// Pour SQLite, c'est `main` et ce que `ATTACH` aurait ajouté : le niveau
/// existe quand même, sans quoi l'arbre aurait deux formes selon le moteur et
/// tout ce qui le parcourt devrait s'en occuper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Database {
    pub name: String,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Table {
    pub name: String,
    /// Une vue et non une table. Un booléen plutôt qu'une énumération de deux
    /// variantes : il n'y a rien d'autre à distinguer ici.
    pub view: bool,
    pub engine: Option<String>,
    /// Approximatif chez MySQL, qui rend l'estimation de l'optimiseur, et
    /// inconnu chez SQLite, qui ne le tient nulle part.
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub name: String,
    /// Le type tel que le moteur le déclare : `varchar(255)`, et non une
    /// abstraction à nous qui perdrait la longueur.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub unique: bool,
    pub auto_increment: bool,
    /// La `table.colonne` visée, quand la colonne est une clé étrangère.
    pub foreign_key: Option<String>,
    pub charset: Option<String>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

/// Une valeur de résultat, déjà mise en texte.
///
/// **`None` est `NULL`, et ce n'est pas la même chose que la chaîne « NULL ».**
/// Une colonne `TEXT` contient couramment le mot, et les confondre se paie
/// trois fois : la table les affiche pareil, l'export CSV écrit `NULL` là où
/// un champ vide est attendu, et le tri en mémoire les range ensemble. Le
/// texte plutôt qu'une valeur typée reste le bon niveau — la vue ne connaît
/// pas les types du moteur, et les lui faire traverser voudrait dire une
/// énumération de valeurs par pilote.
pub type Cell = Option<String>;

/// Le résultat d'une requête, une page à la fois.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// Ce qu'une écriture a touché, quand la requête ne rend pas de lignes.
    pub affected: Option<u64>,
    /// Indice de la première ligne rendue dans le résultat entier.
    pub offset: usize,
    /// Le résultat continue au-delà de cette page.
    pub more: bool,
}

impl Rows {
    /// Ajoute une page à la suite de celle qu'on regarde.
    ///
    /// C'est ce que fait le défilement quand il atteint le bas : la fenêtre
    /// affichée **grandit** au lieu de se déplacer, si bien qu'on ne perd pas
    /// de vue les lignes qu'on venait de lire. Les colonnes sont celles de la
    /// première page — c'est la même requête.
    pub fn extend(&mut self, next: Rows) {
        if self.columns.is_empty() {
            self.columns = next.columns;
        }
        self.rows.extend(next.rows);
        self.more = next.more;
    }
}

/// Les bases d'une connexion.
pub async fn databases(connection: &Connection) -> Result<Vec<Database>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::databases(connection).await,
            Engine::Mysql => mysql::databases(connection).await,
        }
    })
    .await
}

/// Les tables et les vues d'une base.
pub async fn tables(connection: &Connection, database: &str) -> Result<Vec<Table>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::tables(connection, database).await,
            Engine::Mysql => mysql::tables(connection, database).await,
        }
    })
    .await
}

/// Les colonnes d'une table.
pub async fn columns(connection: &Connection, database: &str, table: &str) -> Result<Vec<Column>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::columns(connection, database, table).await,
            Engine::Mysql => mysql::columns(connection, database, table).await,
        }
    })
    .await
}

/// Les colonnes de **toutes** les tables d'une base, par table.
///
/// Une requête et non une par table : indexer un schéma de trois cents tables
/// pour le filtre et les complétions coûterait sinon trois cents connexions.
pub async fn all_columns(
    connection: &Connection,
    database: &str,
) -> Result<BTreeMap<String, Vec<Column>>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::all_columns(connection, database).await,
            Engine::Mysql => mysql::all_columns(connection, database).await,
        }
    })
    .await
}

/// Exécute `sql` et rend la page de `limit` lignes qui commence à `offset`.
///
/// **La pagination se fait en lisant, pas en réécrivant la requête.** Ajouter
/// un `LIMIT` à ce que l'utilisateur a écrit demanderait de comprendre sa
/// requête — un `LIMIT` déjà présent, une union, une procédure — et de la
/// réécrire, ce qui est le plus sûr moyen de lui faire exécuter autre chose
/// que ce qu'il lit. Les lignes qui précèdent la page sont donc bien produites
/// par le moteur, puis jetées ; celles qui suivent ne sont jamais lues.
pub async fn query(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    offset: usize,
    limit: usize,
) -> Result<Rows> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::query(connection, sql, offset, limit).await,
            Engine::Mysql => mysql::query(connection, database, sql, offset, limit).await,
        }
    })
    .await
}

/// Vrai si le résultat de `sql` peut être trié par le moteur.
///
/// Le tri passe par `order_by`, qui **enveloppe** la requête : deux choses
/// l'en empêchent, et il vaut mieux ne pas proposer le geste que le proposer
/// et échouer. Une requête qu'on ne sait pas envelopper, d'abord — voir
/// `order_by`. Deux colonnes de même nom, ensuite : MySQL refuse une table
/// dérivée dont deux colonnes s'appellent pareil, ce qui est le cas courant
/// d'une jointure écrite `SELECT * FROM a JOIN b`.
pub fn can_order(sql: &str, columns: &[String]) -> bool {
    if columns.is_empty() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    if !columns.iter().all(|name| seen.insert(name.to_lowercase())) {
        return false;
    }
    order_by(sql, 0, true).is_some()
}

/// La requête, entourée de quoi la trier sur sa `column`-ième colonne.
///
/// **Cliquer un en-tête demande au moteur, il ne trie pas la page.** Trier en
/// mémoire ce qu'on a sous les yeux mentirait dès la deuxième page : les mille
/// lignes chargées seraient rangées entre elles, et la ligne la plus grande du
/// résultat resterait à la page suivante. C'est la seule chose que Claudhub
/// ajoute à la requête de l'utilisateur, et elle est bornée par tout ce qui
/// suit.
///
/// **La requête n'est pas réécrite, elle est enveloppée** : `SELECT * FROM (…)
/// ORDER BY`. Comprendre la requête pour y insérer un `ORDER BY` — un tri déjà
/// présent, une union, un `LIMIT` — est le plus sûr moyen de lui faire
/// exécuter autre chose que ce qu'elle lit ; une table dérivée, elle, ne
/// change pas le sens de ce qu'elle contient.
///
/// **On ordonne par le rang de la colonne et non par son nom** : un rang ne se
/// cite pas, alors qu'un nom devrait l'être selon des règles propres à chaque
/// moteur, et une colonne calculée s'appelle `count(*)`.
///
/// **Les parenthèses sont sur leur propre ligne**, ce qui met le `)` hors de
/// portée d'un commentaire `--` terminant la requête.
///
/// `None` quand la requête ne se laisse pas envelopper : plusieurs
/// instructions — la parenthèse tomberait entre deux —, ou autre chose qu'une
/// lecture. Le point-virgule est cherché dans le texte brut, si bien qu'une
/// requête portant un `;` dans une chaîne littérale perd le tri : c'est le
/// sens du refus, et il ne coûte qu'un geste indisponible.
pub fn order_by(sql: &str, column: usize, ascending: bool) -> Option<String> {
    let body = sql.trim().trim_end_matches(';').trim_end();
    if body.is_empty() || body.contains(';') {
        return None;
    }
    let head = body
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    // La chaîne vide est le cas d'une requête ouvrant sur une parenthèse,
    // `(SELECT …) UNION (SELECT …)`.
    if !matches!(head.as_str(), "" | "SELECT" | "WITH" | "VALUES" | "TABLE") {
        return None;
    }
    let direction = if ascending { "ASC" } else { "DESC" };
    Some(format!(
        "SELECT * FROM (\n{body}\n) AS claudhub_result ORDER BY {} {direction}",
        column + 1
    ))
}

/// Une ligne de tableau, valeurs échappées et terminée par un saut de ligne.
///
/// L'échappement est celui de la RFC 4180 : on n'encadre que ce qui en a
/// besoin — le séparateur, un guillemet, un saut de ligne —, et un guillemet
/// se double. Le terminateur est un `\n` et non le `\r\n` de la RFC : tout ce
/// qui lit du CSV accepte les deux, et un fichier qu'on ouvre dans son éditeur
/// à côté du code n'a pas à être semé de retours chariot.
///
/// **Une valeur nulle est un champ vide**, ce qui est la convention de tous
/// les exports SQL — et la raison pour laquelle `Cell` distingue `NULL` de la
/// chaîne « NULL », qui sort ici entre guillemets.
pub fn sep_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>, separator: char) -> String {
    let mut line = String::new();
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            line.push(separator);
        }
        let Some(value) = field else { continue };
        if value.contains([separator, '"', '\n', '\r']) {
            line.push('"');
            for c in value.chars() {
                if c == '"' {
                    line.push('"');
                }
                line.push(c);
            }
            line.push('"');
        } else {
            line.push_str(value);
        }
    }
    line.push('\n');
    line
}

/// Une ligne de CSV : ce qui part dans un **fichier**.
pub fn csv_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sep_line(fields, ',')
}

/// Une ligne de valeurs séparées par des tabulations : ce qui part dans le
/// **presse-papiers**.
///
/// Les deux formats ne se distinguent que par leur destination, et c'est ce
/// qui les départage : **un presse-papiers se colle, un fichier s'ouvre.** Un
/// collage tombe dans une grille de tableur ou dans un message, où la
/// tabulation garde les colonnes et où la virgule ne fait qu'une seule
/// cellule d'une ligne entière ; un fichier, lui, est lu par un programme qui
/// sait analyser du CSV. C'est le partage que font DataGrip et PhpStorm.
pub fn tsv_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sep_line(fields, '\t')
}

/// Au-delà, l'export est abandonné.
///
/// Dix fois le délai d'une requête : un export porte sur le résultat
/// **entier** là où la console n'en lit qu'une page, et une table d'un million
/// de lignes met plus de trente secondes à sortir sans que rien n'aille mal.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);

/// Écrit le résultat entier de `sql` dans un fichier CSV, et rend son nombre
/// de lignes.
///
/// **La requête est rejouée, et le résultat n'est jamais tenu en mémoire.**
/// Exporter ce qui est affiché n'exporterait qu'une page — ce n'est jamais ce
/// qu'on veut d'un export — et tout charger pour l'écrire ensuite ferait
/// tenir un million de lignes dans le tas pour les recopier aussitôt. Les
/// lignes vont donc du moteur au fichier une par une.
///
/// L'écriture est **bloquante au milieu d'une tâche asynchrone**, ce qui est
/// assumé : c'est un fichier local, l'exécuteur ne porte que du travail de
/// base de données, et un worker qui écrit attend exactement comme il attend
/// une socket.
pub async fn export_csv(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    path: &std::path::Path,
) -> Result<u64> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("écriture de {} impossible", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let written = tokio::time::timeout(EXPORT_TIMEOUT, async {
        match connection.engine {
            Engine::Sqlite => sqlite::export(connection, sql, &mut out).await,
            Engine::Mysql => mysql::export(connection, database, sql, &mut out).await,
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("l'export n'a pas abouti en {} s", EXPORT_TIMEOUT.as_secs()))??;
    std::io::Write::flush(&mut out)?;
    Ok(written)
}

/// Les octets d'une valeur binaire, en texte.
///
/// MySQL range son type JSON dans un `LONGTEXT` à collation binaire, et les
/// colonnes binaires portent souvent du texte lisible : montrer le texte quand
/// les octets sont de l'UTF-8 valable vaut mieux que de le cacher derrière un
/// compte.
pub(crate) fn bytes_to_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => format!("<{}>", size(error.as_bytes().len() as u64)),
    }
}

/// Un volume en octets, dans l'unité qui lui va. Neutre en langue : ces
/// chaînes apparaissent au milieu de valeurs, pas dans un libellé traduit.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Un nombre de lignes, abrégé : une table de six millions de lignes ne doit
/// pas prendre toute la largeur d'un panneau étroit.
pub fn count(rows: u64) -> String {
    if rows >= 1_000_000 {
        format!("{:.1}M", rows as f64 / 1_000_000.)
    } else if rows >= 10_000 {
        format!("{:.1}k", rows as f64 / 1_000.)
    } else {
        rows.to_string()
    }
}

/// Le chemin d'un fichier SQLite, `~/` développé.
///
/// Un chemin saisi dans un formulaire s'écrit `~/dev/base.sqlite` — c'est
/// ainsi qu'on le donne à un shell — et le passer tel quel à `std::fs`
/// chercherait un dossier nommé `~` dans le répertoire courant.
pub(crate) fn expand(path: &str) -> std::path::PathBuf {
    match path.trim().strip_prefix("~/") {
        Some(rest) => match directories::UserDirs::new() {
            Some(dirs) => dirs.home_dir().join(rest),
            None => std::path::PathBuf::from(path.trim()),
        },
        None => std::path::PathBuf::from(path.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::executor::block_on;
    use sqlx::ConnectOptions as _;

    /// Les valeurs d'un résultat, le nul rendu visible.
    fn shown(rows: &Rows) -> Vec<Vec<&str>> {
        rows.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.as_deref().unwrap_or("<null>"))
                    .collect()
            })
            .collect()
    }

    fn sqlite_at(path: &std::path::Path) -> Connection {
        Connection {
            name: String::new(),
            engine: Engine::Sqlite,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    /// Le tour complet sur une vraie base : les trois niveaux de l'arbre, ce
    /// qu'une colonne déclare, et la pagination d'une requête.
    ///
    /// Le test passe par `block_on`, donc par l'exécuteur partagé : c'est le
    /// même pont que celui des workers, et c'est lui qu'on veut éprouver.
    #[test]
    fn sqlite_introspection_and_paging() {
        let path = std::env::temp_dir().join(format!("claudhub-db-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let connection = sqlite_at(&path);

        block_on(async {
            // La base est créée par une connexion **en écriture** : celle que
            // le module ouvre est en lecture seule, et c'est justement ce que
            // le dernier cas vérifie.
            let mut writable = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE users (
                     id INTEGER PRIMARY KEY,
                     email TEXT NOT NULL UNIQUE,
                     name TEXT DEFAULT 'anon');
                 CREATE TABLE posts (
                     id INTEGER PRIMARY KEY,
                     user_id INTEGER NOT NULL REFERENCES users(id));
                 CREATE VIEW v_users AS SELECT * FROM users;
                 INSERT INTO users (email, name)
                     VALUES ('a@example.com', 'Ada'), ('b@example.com', NULL);",
            )
            .execute(&mut writable)
            .await
            .unwrap();
            drop(writable);

            let dbs = databases(&connection).await.unwrap();
            assert_eq!(
                dbs.iter().map(|db| db.name.as_str()).collect::<Vec<_>>(),
                ["main"]
            );

            let listed = tables(&connection, "main").await.unwrap();
            assert_eq!(
                listed
                    .iter()
                    .map(|t| (t.name.as_str(), t.view))
                    .collect::<Vec<_>>(),
                [("posts", false), ("users", false), ("v_users", true)]
            );

            let users = columns(&connection, "main", "users").await.unwrap();
            assert_eq!(users.len(), 3);
            assert!(users[0].primary_key && users[0].name == "id");
            assert!(!users[1].nullable && users[1].unique);
            assert_eq!(users[2].default.as_deref(), Some("'anon'"));

            let posts = columns(&connection, "main", "posts").await.unwrap();
            assert_eq!(posts[1].foreign_key.as_deref(), Some("users.id"));

            let all = all_columns(&connection, "main").await.unwrap();
            assert_eq!(all.len(), 3);
            assert_eq!(all["posts"][1].foreign_key.as_deref(), Some("users.id"));

            let page = query(
                &connection,
                None,
                "SELECT id, email, name FROM users ORDER BY id",
                0,
                1000,
            )
            .await
            .unwrap();
            assert_eq!(page.columns, ["id", "email", "name"]);
            // La seconde ligne a un `name` nul, et c'est **`None`** et non la
            // chaîne « NULL » : tout l'export et tout le tri en dépendent.
            assert_eq!(
                shown(&page),
                [
                    ["1", "a@example.com", "Ada"],
                    ["2", "b@example.com", "<null>"]
                ]
            );
            assert!(!page.more);

            let first = query(&connection, None, "SELECT id FROM users ORDER BY id", 0, 1)
                .await
                .unwrap();
            assert_eq!(shown(&first), [["1"]]);
            assert!(first.more, "une page pleine annonce la suivante");

            let second = query(&connection, None, "SELECT id FROM users ORDER BY id", 1, 1)
                .await
                .unwrap();
            assert_eq!(shown(&second), [["2"]]);
            assert_eq!(second.offset, 1);
            assert!(!second.more);

            // Le fichier est ouvert en lecture seule : une écriture doit
            // échouer, et c'est le moteur qui le dit.
            assert!(query(
                &connection,
                None,
                "INSERT INTO users (email) VALUES ('c@example.com')",
                0,
                10
            )
            .await
            .is_err());
        });

        let _ = std::fs::remove_file(&path);
    }

    /// Trier et exporter, sur la même base : les deux rejouent la requête, et
    /// c'est le seul endroit où l'on puisse vérifier que ce qu'on lui ajoute
    /// est du SQL que le moteur accepte.
    #[test]
    fn sorting_and_exporting_replay_the_query() {
        let path = std::env::temp_dir().join(format!("claudhub-csv-{}.sqlite", std::process::id()));
        let csv = path.with_extension("csv");
        let _ = std::fs::remove_file(&path);
        let connection = sqlite_at(&path);

        block_on(async {
            let mut writable = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT);
                 INSERT INTO t (label) VALUES ('a, virgule'), (NULL), ('gu\"illemet');",
            )
            .execute(&mut writable)
            .await
            .unwrap();
            drop(writable);

            // Le tri est demandé au moteur autour de la requête, y compris
            // quand elle porte son propre point-virgule.
            let sorted = order_by("SELECT id, label FROM t;", 0, false).unwrap();
            let page = query(&connection, None, &sorted, 0, 10).await.unwrap();
            assert_eq!(
                shown(&page).iter().map(|row| row[0]).collect::<Vec<_>>(),
                ["3", "2", "1"]
            );

            let written = export_csv(&connection, None, "SELECT id, label FROM t", &csv)
                .await
                .unwrap();
            assert_eq!(written, 3);
        });

        // L'en-tête, le nul rendu par un champ vide, et les deux échappements.
        assert_eq!(
            std::fs::read_to_string(&csv).unwrap(),
            "id,label\n1,\"a, virgule\"\n2,\n3,\"gu\"\"illemet\"\n"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&csv);
    }

    /// La requête n'est pas réécrite : elle est mise dans une table dérivée,
    /// et le rang de la colonne évite d'avoir à citer son nom.
    #[test]
    fn a_query_is_wrapped_to_be_sorted() {
        let wrapped = order_by("SELECT a, b FROM t  ;\n", 1, true).unwrap();
        assert_eq!(
            wrapped,
            "SELECT * FROM (\nSELECT a, b FROM t\n) AS claudhub_result ORDER BY 2 ASC"
        );
        assert!(order_by("select 1", 0, false).unwrap().ends_with("1 DESC"));
        // Une parenthèse ouvrante en tête, c'est une union parenthésée.
        assert!(order_by("(SELECT 1) UNION (SELECT 2)", 0, true).is_some());
        // La parenthèse fermante est sur sa propre ligne, hors de portée d'un
        // commentaire qui terminerait la requête.
        let commented = order_by("SELECT a FROM t -- tout est là", 0, true).unwrap();
        assert!(commented.contains("\n)"), "{commented}");
    }

    /// Ce qu'on ne sait pas envelopper n'est pas trié — plutôt que trié faux.
    #[test]
    fn what_cannot_be_wrapped_is_not_sorted() {
        assert!(order_by("", 0, true).is_none());
        assert!(order_by("UPDATE t SET a = 1", 0, true).is_none());
        // Deux instructions : la parenthèse tomberait entre les deux.
        assert!(order_by("SELECT 1; SELECT 2", 0, true).is_none());

        let columns = ["id".to_string(), "name".to_string()];
        assert!(can_order("SELECT id, name FROM t", &columns));
        assert!(!can_order("SELECT id, name FROM t", &[]));
        // MySQL refuse une table dérivée à deux colonnes de même nom, ce
        // qu'une jointure écrite `SELECT *` produit tout le temps.
        let doubled = ["id".to_string(), "ID".to_string()];
        assert!(!can_order("SELECT * FROM a JOIN b", &doubled));
    }

    /// Un CSV se relit : c'est l'échappement qui le garantit, et le nul y est
    /// un champ vide et non le mot « NULL ».
    #[test]
    fn csv_quotes_only_what_needs_it() {
        assert_eq!(csv_line([Some("a"), Some("b")]), "a,b\n");
        assert_eq!(csv_line([None, Some("")]), ",\n");
        assert_eq!(csv_line([Some("a,b")]), "\"a,b\"\n");
        assert_eq!(csv_line([Some("dit \"oui\"")]), "\"dit \"\"oui\"\"\"\n");
        assert_eq!(csv_line([Some("deux\nlignes")]), "\"deux\nlignes\"\n");
        // La chaîne « NULL » n'est pas un nul, et elle ne doit pas en devenir
        // un en passant par le CSV.
        assert_eq!(csv_line([Some("NULL")]), "NULL\n");
        assert_eq!(csv_line([None]), "\n");
        // Le presse-papiers prend des tabulations, et n'encadre donc pas une
        // valeur qui porte une virgule — mais bien celle qui porte une
        // tabulation.
        assert_eq!(tsv_line([Some("a,b"), Some("c")]), "a,b\tc\n");
        assert_eq!(tsv_line([Some("a\tb")]), "\"a\tb\"\n");
    }

    /// Prolonger la fenêtre garde ses colonnes et reprend la suite.
    #[test]
    fn a_window_grows_by_its_end() {
        let mut first = Rows {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            more: true,
            ..Default::default()
        };
        first.extend(Rows {
            columns: vec!["id".into()],
            rows: vec![vec![Some("2".into())]],
            offset: 1,
            more: false,
            ..Default::default()
        });
        assert_eq!(shown(&first), [["1"], ["2"]]);
        assert!(!first.more, "la suite dit s'il en reste");
    }

    /// Une base qui n'existe pas est une erreur, pas une attente : le message
    /// est ce que la ligne de l'arbre affichera.
    #[test]
    fn a_missing_file_says_so() {
        let connection = sqlite_at(std::path::Path::new("/tmp/claudhub-aucune-base.sqlite"));
        let error = block_on(databases(&connection)).unwrap_err();
        assert!(error.to_string().contains("aucun fichier"), "{error}");
    }

    #[test]
    fn a_connection_without_a_name_takes_the_one_of_its_address() {
        let file = Connection {
            engine: Engine::Sqlite,
            path: "/srv/app/database.sqlite".into(),
            ..Default::default()
        };
        assert_eq!(file.label(), "database.sqlite");

        let server = Connection {
            engine: Engine::Mysql,
            host: "db.example.com".into(),
            user: "app".into(),
            ..Default::default()
        };
        assert_eq!(server.label(), "app@db.example.com");
        assert_eq!(server.detail(), "db.example.com:3306");
    }

    /// Le mot de passe ne fait pas partie de l'identité d'une connexion : le
    /// corriger dans les réglages ne doit pas refermer l'arbre qu'on avait
    /// déplié.
    #[test]
    fn the_key_ignores_the_password() {
        let mut connection = Connection {
            engine: Engine::Mysql,
            host: "localhost".into(),
            user: "root".into(),
            password: "hunter2".into(),
            ..Default::default()
        };
        let before = connection.key();
        connection.password = "autre".into();
        assert_eq!(before, connection.key());
    }

    /// Le mot de passe ne doit apparaître nulle part dans une trace : c'est ce
    /// qui autorise la connexion entière à voyager dans une `Cmd`.
    #[test]
    fn the_debug_output_hides_the_password() {
        let connection = Connection {
            engine: Engine::Mysql,
            password: "hunter2".into(),
            ..Default::default()
        };
        let shown = format!("{connection:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn volumes_are_abbreviated() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(2048), "2.0 KB");
        assert_eq!(count(42), "42");
        assert_eq!(count(2_500_000), "2.5M");
    }
}
