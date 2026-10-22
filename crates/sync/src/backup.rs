//! `portfolio-sync backup`: a one-off, consistent copy of the database into a directory, named
//! after the day (`portfolio-YYYY-MM-DD.db`). Used by compose's `backup` service (BACKLOG P37),
//! which writes into `./backups/` on the host.
//!
//! The copy is made with SQLite's `VACUUM INTO`, which reads one consistent snapshot even while
//! the dashboard or the daily sync has the database open, so the containers do not need to be
//! stopped first. An existing backup is never overwritten: a second copy on the same day gets a
//! `-2`, `-3`, … suffix.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::NaiveDate;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// Default directory, relative to the working directory.
pub const DEFAULT_DIR: &str = "backups";

/// More copies than this on one day means something is looping; refuse instead of guessing.
const MAX_PER_DAY: u32 = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOptions {
    /// Directory to write the copy into; it must exist.
    pub dir: PathBuf,
}

/// Parses the arguments after `backup`.
pub fn parse_args<I, S>(args: I) -> Result<BackupOptions, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = BackupOptions {
        dir: PathBuf::from(DEFAULT_DIR),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--out" => {
                let value = args.next().ok_or("--out needs a directory")?;
                if value.as_ref().is_empty() {
                    return Err("--out needs a directory".to_owned());
                }
                options.dir = PathBuf::from(value.as_ref());
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(options)
}

/// The first free name for a backup made on `date` in `dir`: `portfolio-2026-10-22.db`, then
/// `portfolio-2026-10-22-2.db`, and so on. `exists` says whether a path is taken.
pub fn backup_path(
    dir: &Path,
    date: NaiveDate,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, BackupError> {
    let day = date.format("%Y-%m-%d");
    for n in 1..=MAX_PER_DAY {
        let name = if n == 1 {
            format!("portfolio-{day}.db")
        } else {
            format!("portfolio-{day}-{n}.db")
        };
        let path = dir.join(name);
        if !exists(&path) {
            return Ok(path);
        }
    }
    Err(BackupError::TooMany {
        dir: dir.to_owned(),
        date,
    })
}

#[derive(Debug)]
pub enum BackupError {
    /// The database to back up is not there (a wrong working directory or `PORTFOLIO_DB_URL`).
    NoDatabase(PathBuf),
    Open(sqlx::Error),
    NoDir(PathBuf),
    TooMany {
        dir: PathBuf,
        date: NaiveDate,
    },
    NotUtf8(PathBuf),
    Copy(sqlx::Error),
    Permissions(PathBuf, std::io::Error),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupError::NoDatabase(p) => {
                write!(f, "no database at {}; nothing to back up", p.display())
            }
            BackupError::Open(e) => write!(f, "cannot open the database: {e}"),
            BackupError::NoDir(d) => write!(f, "backup directory {} does not exist", d.display()),
            BackupError::TooMany { dir, date } => write!(
                f,
                "{MAX_PER_DAY} backups for {date} already in {}; remove some first",
                dir.display()
            ),
            BackupError::NotUtf8(p) => write!(f, "backup path {} is not UTF-8", p.display()),
            BackupError::Copy(e) => write!(f, "cannot copy the database: {e}"),
            BackupError::Permissions(p, e) => {
                write!(f, "cannot make {} private: {e}", p.display())
            }
        }
    }
}

impl std::error::Error for BackupError {}

/// Opens the database to back up at `url`. Unlike `portfolio_db::open`, it never creates the
/// file and runs no migrations: a backup of a database that is not there would otherwise
/// report success for a fresh, empty copy and leave a stray empty database behind.
pub async fn open_source(url: &str) -> Result<SqlitePool, BackupError> {
    let options = SqliteConnectOptions::from_str(url)
        .map_err(BackupError::Open)?
        .create_if_missing(false);
    let file = options.get_filename();
    if !file.is_file() {
        return Err(BackupError::NoDatabase(file.to_owned()));
    }
    SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(BackupError::Open)
}

/// Writes a consistent copy of the database behind `pool` into `options.dir` and returns its
/// path. The file is readable by its owner only, since it holds the whole portfolio.
pub async fn backup(
    pool: &SqlitePool,
    options: &BackupOptions,
    date: NaiveDate,
) -> Result<PathBuf, BackupError> {
    if !options.dir.is_dir() {
        return Err(BackupError::NoDir(options.dir.clone()));
    }
    let path = backup_path(&options.dir, date, Path::exists)?;
    let target = path
        .to_str()
        .ok_or_else(|| BackupError::NotUtf8(path.clone()))?;
    sqlx::query("VACUUM INTO ?")
        .bind(target)
        .execute(pool)
        .await
        .map_err(BackupError::Copy)?;
    make_private(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn make_private(path: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| BackupError::Permissions(path.to_owned(), e))
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    /// A fresh, empty directory under the system temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "portfolio-backup-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn args_default_and_out() {
        assert_eq!(
            parse_args(Vec::<String>::new()),
            Ok(BackupOptions {
                dir: PathBuf::from("backups")
            })
        );
        assert_eq!(
            parse_args(["--out", "/backups"]),
            Ok(BackupOptions {
                dir: PathBuf::from("/backups")
            })
        );
        assert!(parse_args(["--out"]).is_err());
        assert!(parse_args(["--out", ""]).is_err());
        assert!(parse_args(["--date", "2026-10-22"]).is_err());
    }

    #[test]
    fn first_backup_of_the_day_has_the_plain_name() {
        let path = backup_path(Path::new("/b"), d("2026-10-22"), |_| false).unwrap();
        assert_eq!(path, PathBuf::from("/b/portfolio-2026-10-22.db"));
    }

    #[test]
    fn later_backups_on_the_same_day_are_numbered() {
        // The plain name and -2 are taken, so the third copy is -3.
        let taken = [
            PathBuf::from("/b/portfolio-2026-10-22.db"),
            PathBuf::from("/b/portfolio-2026-10-22-2.db"),
            // Another day's copy does not count.
            PathBuf::from("/b/portfolio-2026-10-21-3.db"),
        ];
        let path = backup_path(Path::new("/b"), d("2026-10-22"), |p| {
            taken.iter().any(|t| t == p)
        })
        .unwrap();
        assert_eq!(path, PathBuf::from("/b/portfolio-2026-10-22-3.db"));
    }

    #[test]
    fn refuses_after_the_daily_cap() {
        let err = backup_path(Path::new("/b"), d("2026-10-22"), |_| true).unwrap_err();
        assert!(matches!(err, BackupError::TooMany { .. }), "{err}");
    }

    async fn count_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM backup_probe")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn copies_the_data_and_never_overwrites() {
        let tmp = TempDir::new("copy");
        let db_url = format!("sqlite://{}", tmp.0.join("portfolio.db").display());
        let pool = portfolio_db::open(&db_url).await.unwrap();
        // Two rows the copy must hold, next to the migrated schema.
        sqlx::query("CREATE TABLE backup_probe (v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO backup_probe (v) VALUES ('a'), ('b')")
            .execute(&pool)
            .await
            .unwrap();
        let out = tmp.0.join("backups");
        std::fs::create_dir(&out).unwrap();
        let options = BackupOptions { dir: out.clone() };

        let first = backup(&pool, &options, d("2026-10-22")).await.unwrap();
        assert_eq!(first, out.join("portfolio-2026-10-22.db"));
        let second = backup(&pool, &options, d("2026-10-22")).await.unwrap();
        assert_eq!(second, out.join("portfolio-2026-10-22-2.db"));

        let copy = portfolio_db::open(&format!("sqlite://{}", first.display()))
            .await
            .unwrap();
        assert_eq!(count_rows(&copy).await, 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn missing_database_is_an_error_and_is_not_created() {
        let tmp = TempDir::new("nodb");
        let file = tmp.0.join("portfolio.db");
        let err = open_source(&format!("sqlite://{}", file.display()))
            .await
            .unwrap_err();
        assert!(matches!(err, BackupError::NoDatabase(_)), "{err}");
        assert!(!file.exists(), "a missing database must not be created");
    }

    #[tokio::test]
    async fn open_source_reads_an_existing_database() {
        let tmp = TempDir::new("open");
        let url = format!("sqlite://{}", tmp.0.join("portfolio.db").display());
        let pool = portfolio_db::open(&url).await.unwrap();
        sqlx::query("CREATE TABLE backup_probe (v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO backup_probe (v) VALUES ('a')")
            .execute(&pool)
            .await
            .unwrap();
        let source = open_source(&url).await.unwrap();
        assert_eq!(count_rows(&source).await, 1);
    }

    #[tokio::test]
    async fn missing_directory_is_an_error() {
        let tmp = TempDir::new("nodir");
        let pool = portfolio_db::open_in_memory().await.unwrap();
        let options = BackupOptions {
            dir: tmp.0.join("absent"),
        };
        let err = backup(&pool, &options, d("2026-10-22")).await.unwrap_err();
        assert!(matches!(err, BackupError::NoDir(_)), "{err}");
    }
}
