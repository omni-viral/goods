use {
    crate::{asset::Asset, import::Importers},
    std::{
        io::Read,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::SystemTime,
    },
    uuid::Uuid,
};

/// Storage for goods.
pub struct Treasury {
    registry: Arc<Mutex<Registry>>,
}

#[derive(PartialEq, Hash)]
struct Kind {
    source_path: Arc<Path>,
    source_format: Arc<str>,
    native_format: Arc<str>,
}

pub(crate) struct Registry {
    /// All paths not suffixed with `_absolute` are relative to this.
    root: Box<Path>,

    // Data loaded from `root.join(".treasury/db")`.
    data: Data,

    /// Importers
    importers: Importers,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Data {
    importers_dirs: Vec<Box<Path>>,
    /// Array with all registered assets.
    assets: Vec<Asset>,
}

pub struct AssetData {
    pub bytes: Box<[u8]>,
    pub version: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum NewError {
    #[error("Goods path '{path}' is occupied")]
    GoodsAlreadyExist { path: Box<Path> },

    #[error("Goods path '{path}' is invalid")]
    InvalidGoodsPath { path: Box<Path> },

    #[error("Failed to create root directory '{path}'")]
    RootDirCreateError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Failed to create goods directory '{path}'")]
    GoodsDirCreateError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Root '{path}' is not a directory")]
    RootIsNotDir { path: Box<Path> },

    #[error(transparent)]
    SaveError(#[from] SaveError),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("Failed to open goods path '{path}'")]
    GoodsOpenError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Failed to deserialize goods file")]
    JsonError {
        path: Box<Path>,
        source: serde_json::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("Goods path '{path}' is invalid")]
    InvalidGoodsPath { path: Box<Path> },

    #[error("Failed to create directory for goods path '{path}'")]
    GoodsCreateError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Failed to open goods path '{path}'")]
    GoodsOpenError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Failed to deserialize goods file")]
    JsonError {
        path: Box<Path>,
        source: serde_json::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("Asset not found")]
    NotFound,

    #[error("Failed to access native file '{path}'")]
    NativeIoError {
        path: Box<Path>,
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("No importer from '{source_format}' to '{native_format}' found")]
    ImporterNotFound {
        source_format: String,
        native_format: String,
    },

    #[error("Import failed")]
    ImportError { source: eyre::Report },

    #[error("Failed to access source file '{path}'")]
    SourceIoError {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("Failed to access native file '{path}'")]
    NativeIoError {
        path: Box<Path>,
        source: std::io::Error,
    },
}

impl Treasury {
    /// Create new goods storage.
    #[tracing::instrument(fields(root = %root.as_ref().display()))]
    pub fn new(root: impl AsRef<Path>, overwrite: bool) -> Result<Self, NewError> {
        let root = root.as_ref();

        if !root.exists() {
            std::fs::create_dir_all(&root).map_err(|source| NewError::RootDirCreateError {
                source,
                path: root.into(),
            })?;
        } else if !root.is_dir() {
            return Err(NewError::RootIsNotDir { path: root.into() });
        }

        let treasury_path = root.join(".treasury");

        if treasury_path.exists() {
            if treasury_path.is_file() {
                return Err(NewError::GoodsAlreadyExist {
                    path: treasury_path.into(),
                });
            }

            let manifest_path = treasury_path.join("manifest.json");
            if !overwrite && manifest_path.exists() {
                return Err(NewError::GoodsAlreadyExist {
                    path: treasury_path.into(),
                });
            }
        } else if let Err(err) = std::fs::create_dir(&treasury_path) {
            return Err(NewError::GoodsDirCreateError {
                source: err,
                path: treasury_path.into(),
            });
        }

        let goods = Treasury {
            registry: Arc::new(Mutex::new(Registry {
                importers: Importers::new(&root),
                root: root.into(),
                data: Data {
                    assets: Vec::new(),
                    importers_dirs: Vec::new(),
                },
            })),
        };

        // let file =
        //     std::fs::File::create(&treasury_path.join("manifest.json")).map_err(|source| {
        //         SaveError::GoodsOpenError {
        //             source,
        //             path: treasury_path.clone().into(),
        //         }
        //     })?;

        // serde_json::to_writer(file, &goods.inner.data).map_err(|source| SaveError::JsonError {
        //     source,
        //     path: treasury_path.clone().into(),
        // })?;

        Ok(goods)
    }

    /// Opens goods storage from metadata file.
    #[tracing::instrument(skip(root), fields(root = %root.as_ref().display()))]
    pub fn open(root: impl AsRef<Path>) -> Result<Self, OpenError> {
        let root = root.as_ref();

        let treasury_path = root.join(".treasury");
        let manifest_path = treasury_path.join("manifest.json");

        let file =
            std::fs::File::open(&manifest_path).map_err(|source| OpenError::GoodsOpenError {
                source,
                path: manifest_path.clone().into(),
            })?;

        let mut data: Data =
            serde_json::from_reader(file).map_err(|source| OpenError::JsonError {
                source,
                path: manifest_path.clone().into(),
            })?;

        for asset in &mut data.assets {
            asset.update_abs_paths(&root);
        }

        let registry = Arc::new(Mutex::new(Registry {
            importers: Importers::new(&root),
            data,
            root: root.into(),
        }));

        let registry_clone = registry.clone();

        let mut lock = registry.lock().unwrap();
        let me = &mut *lock;

        for dir_path in &me.data.importers_dirs {
            let root_dir_path = me.root.join(dir_path);
            if let Err(err) = me
                .importers
                .load_importers_dir(&root_dir_path, &registry_clone)
            {
                tracing::error!(
                    "Failed to load importers from '{} ({})'. {:#}",
                    dir_path.display(),
                    root_dir_path.display(),
                    err
                );
            }
        }
        drop(lock);

        Ok(Treasury { registry })
    }

    pub fn save(&self) -> Result<(), SaveError> {
        Registry::save(&self.registry)
    }

    pub fn load_importers_dir(&mut self, dir_path: &Path) -> std::io::Result<()> {
        if self
            .registry
            .lock()
            .unwrap()
            .data
            .importers_dirs
            .iter()
            .any(|d| **d == *dir_path)
        {
            Ok(())
        } else {
            let registry_clone = self.registry.clone();

            let mut lock = self.registry.lock().unwrap();

            match lock.importers.load_importers_dir(dir_path, &registry_clone) {
                Ok(()) => {
                    let dir_path = relative_to(dir_path, &lock.root);

                    lock.data
                        .importers_dirs
                        .push(dir_path.into_owned().into_boxed_path());
                    Ok(())
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to load importers from '{}'. {:#}",
                        dir_path.display(),
                        err
                    );
                    Err(err)
                }
            }
        }
    }

    /// Import asset into goods instance
    pub fn store(
        &self,
        source: impl AsRef<Path>,
        source_format: &str,
        native_format: &str,
        tags: &[impl AsRef<str>],
    ) -> Result<Uuid, StoreError> {
        Registry::store(
            &self.registry,
            source.as_ref(),
            source_format,
            native_format,
            tags,
        )
    }

    /// Fetches asset in native format.
    /// Performs conversion if native format is absent or out of date.
    #[tracing::instrument(skip(self))]
    pub fn fetch(&mut self, uuid: &Uuid) -> Result<AssetData, FetchError> {
        match Registry::fetch(&self.registry, uuid, 0)? {
            None => unreachable!(),
            Some(mut info) => {
                let mut bytes = Vec::new();
                info.native_file.read_to_end(&mut bytes).map_err(|source| {
                    FetchError::NativeIoError {
                        source,
                        path: info.native_path.to_path_buf().into(),
                    }
                })?;

                Ok(AssetData {
                    bytes: bytes.into_boxed_slice(),
                    version: info.version,
                })
            }
        }
    }

    /// Fetches asset in native format.
    /// Returns `Ok(None)` if native file is up-to-date.
    /// Performs conversion if native format is absent or out of date.
    #[tracing::instrument(skip(self))]
    pub fn fetch_updated(
        &mut self,
        uuid: &Uuid,
        version: u64,
    ) -> Result<Option<AssetData>, FetchError> {
        match Registry::fetch(&self.registry, uuid, version + 1)? {
            None => Ok(None),
            Some(mut info) => {
                let mut bytes = Vec::new();
                info.native_file.read_to_end(&mut bytes).map_err(|source| {
                    FetchError::NativeIoError {
                        source,
                        path: info.native_path.to_path_buf().into(),
                    }
                })?;

                Ok(Some(AssetData {
                    bytes: bytes.into_boxed_slice(),
                    version: info.version,
                }))
            }
        }
    }

    /// Returns assets information.
    #[tracing::instrument(skip(self, tags))]
    pub fn list(&self, tags: &[impl AsRef<str>], native_format: Option<&str>) -> Vec<Asset> {
        let lock = self.registry.lock().unwrap();

        lock.data
            .assets
            .iter()
            .filter(|a| {
                if let Some(native_format) = native_format {
                    if a.native_format() != native_format {
                        return false;
                    }
                }

                tags.iter().all(|tag| {
                    let tag = tag.as_ref();
                    a.tags().iter().any(|t| **t == *tag)
                })
            })
            .cloned()
            .collect()
    }

    /// Returns assets information.
    #[tracing::instrument(skip(self))]
    pub fn remove<'a>(&self, uuid: Uuid) {
        let mut lock = self.registry.lock().unwrap();

        if let Some(index) = lock.data.assets.iter().position(|a| a.uuid() == uuid) {
            let asset = &lock.data.assets[index];
            if let Err(err) = std::fs::remove_file(asset.native_absolute()) {
                tracing::error!("Failed to remove native asset file '{}'", err);
            }
            lock.data.assets.remove(index);
        }
    }
}

pub(crate) struct FetchInfo {
    pub native_path: Box<Path>,
    pub native_file: std::fs::File,
    pub version: u64,
}

impl Registry {
    fn save(me: &Mutex<Self>) -> Result<(), SaveError> {
        let lock = me.lock().unwrap();
        let treasury_path = lock.root.join(".treasury").join("manifest.json");
        let file =
            std::fs::File::create(&treasury_path).map_err(|source| SaveError::GoodsOpenError {
                source,
                path: treasury_path.clone().into(),
            })?;
        serde_json::to_writer_pretty(file, &lock.data).map_err(|source| SaveError::JsonError {
            source,
            path: treasury_path.into(),
        })
    }

    pub(crate) fn store(
        me: &Mutex<Self>,
        source: &Path,
        source_format: &str,
        native_format: &str,
        tags: &[impl AsRef<str>],
    ) -> Result<Uuid, StoreError> {
        let mut lock = me.lock().unwrap();

        // Find the source
        let cd = std::env::current_dir().map_err(|_| StoreError::SourceIoError {
            path: source.into(),
            source: std::io::ErrorKind::NotFound.into(),
        })?;

        let source_absolute = cd.join(source);

        let source_from_root = relative_to(&source_absolute, &lock.root)
            .into_owned()
            .into_boxed_path();

        if let Some(asset) = lock.data.assets.iter().find(|a| {
            *a.source() == *source_from_root
                && a.source_format() == source_format
                && a.native_format() == native_format
        }) {
            tracing::trace!("Already imported");
            return Ok(asset.uuid());
        }

        tracing::debug!(
            "Importing {} as {} @ {}",
            source_format,
            native_format,
            source.display()
        );

        let uuid = loop {
            let uuid = Uuid::new_v4();
            if !lock.data.assets.iter().any(|a| a.uuid() == uuid) {
                break uuid;
            }
        };

        let native = Path::new(".treasury").join(uuid.to_hyphenated().to_string());
        let native_absolute = lock.root.join(&native);

        if source_format == native_format {
            if let Err(err) = std::fs::copy(&source_absolute, &native_absolute) {
                return Err(StoreError::SourceIoError {
                    source: err,
                    path: source_absolute.into(),
                });
            }
        } else {
            match lock.importers.get_importer(source_format, native_format) {
                None => {
                    return Err(StoreError::ImporterNotFound {
                        source_format: source_format.to_owned(),
                        native_format: native_format.to_owned(),
                    })
                }
                Some(importer_entry) => {
                    tracing::trace!("Importer found. {}", importer_entry.name());

                    let native_tmp_path = native.with_extension("tmp");
                    let native_tmp_path_absolute = native_absolute.with_extension("tmp");

                    let result = importer_entry.import(
                        &source_absolute,
                        &relative_to(&native_tmp_path, &lock.root),
                        lock,
                    );

                    if let Err(err) = result {
                        return Err(StoreError::ImportError { source: err });
                    }

                    tracing::trace!("Imported successfully");
                    if let Err(err) = std::fs::rename(&native_tmp_path_absolute, &native_absolute) {
                        tracing::error!(
                            "Failed to rename '{}' to '{}'",
                            native_tmp_path.display(),
                            native_absolute.display(),
                        );

                        return Err(StoreError::NativeIoError {
                            path: native_absolute.into(),
                            source: err,
                        });
                    }

                    lock = me.lock().unwrap();
                }
            }
        }

        lock.data.assets.push(Asset::new(
            uuid,
            source_from_root,
            source_format.into(),
            native_format.into(),
            tags.iter().map(|tag| tag.as_ref().into()).collect(),
            native_absolute.into(),
            source_absolute.into(),
        ));

        tracing::info!("Asset '{}' registered", uuid);
        drop(lock);
        let _ = Self::save(me);

        Ok(uuid)
    }

    pub(crate) fn fetch(
        me: &Mutex<Self>,
        uuid: &Uuid,
        next_version: u64,
    ) -> Result<Option<FetchInfo>, FetchError> {
        let mut lock = me.lock().unwrap();

        match lock.data.assets.iter().position(|a| a.uuid() == *uuid) {
            None => Err(FetchError::NotFound),
            Some(index) => {
                let mut asset = &lock.data.assets[index];
                let mut native_path = asset.native_absolute();
                let mut native_file = std::fs::File::open(native_path).map_err(|source| {
                    FetchError::NativeIoError {
                        source,
                        path: native_path.to_path_buf().into(),
                    }
                })?;

                let native_modified =
                    native_file
                        .metadata()
                        .and_then(|m| m.modified())
                        .map_err(|source| FetchError::NativeIoError {
                            source,
                            path: native_path.to_path_buf().into(),
                        })?;

                if let Ok(source_modified) =
                    std::fs::metadata(asset.source_absolute()).and_then(|m| m.modified())
                {
                    if native_modified < source_modified {
                        tracing::trace!("Native asset file is out-of-date. Perform reimport");

                        match lock
                            .importers
                            .get_importer(asset.source_format(), asset.native_format())
                        {
                            None => {
                                tracing::warn!(
                                    "Importer from '{}' to '{}' not found, asset '{}@{}' cannot be updated",
                                    asset.source_format(),
                                    asset.native_format(),
                                    asset.uuid(),
                                    asset.source().display(),
                                );
                            }
                            Some(importer) => {
                                let native_tmp_path = native_path.with_extension("tmp");

                                let result = importer.import(
                                    &asset.source_absolute().to_owned(),
                                    &relative_to(&native_tmp_path, &lock.root),
                                    lock,
                                );

                                lock = me.lock().unwrap();
                                asset = &lock.data.assets[index];

                                native_path = asset.native_absolute();

                                match result {
                                    Ok(()) => {
                                        drop(native_file);
                                        match std::fs::rename(&native_tmp_path, native_path) {
                                            Ok(()) => {
                                                tracing::trace!("Native file updated");
                                            }
                                            Err(err) => {
                                                tracing::warn!(
                                                            "Failed to copy native file '{}' from '{}'. {:#}",
                                                            native_path.display(),
                                                            native_tmp_path.display(),
                                                            err
                                                        )
                                            }
                                        }
                                        match std::fs::File::open(native_path) {
                                            Ok(file) => native_file = file,
                                            Err(err) => {
                                                tracing::warn!(
                                                    "Failed to reopen native file '{}'. {:#}",
                                                    native_path.display(),
                                                    err,
                                                );
                                                return Err(FetchError::NativeIoError {
                                                    source: err,
                                                    path: native_path.to_path_buf().into(),
                                                });
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            "Native file reimport failed '{:#}'. Fallback to old file",
                                            err,
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        tracing::trace!("Native asset file is up-to-date");
                    }
                } else {
                    tracing::warn!("Failed to determine if native file is up-to-date");
                }

                let version = version_from_systime(native_modified);
                if next_version > version {
                    tracing::trace!("Native asset is not updated");
                    return Ok(None);
                }

                let native_path = lock.data.assets[index].native_absolute().into();

                Ok(Some(FetchInfo {
                    native_path,
                    native_file,
                    version,
                }))
            }
        }
    }
}

fn relative_to<'a>(path: &'a Path, root: &Path) -> std::borrow::Cow<'a, Path> {
    debug_assert!(path.is_absolute());
    debug_assert!(root.is_absolute());

    let mut pcs = path.components();
    let mut rcs = root.components();

    let prefix_length = pcs
        .by_ref()
        .zip(&mut rcs)
        .take_while(|(pc, rc)| pc == rc)
        .count();

    if prefix_length == 0 {
        path.into()
    } else {
        let mut pcs = path.components();
        pcs.nth(prefix_length - 1);

        let mut rcs = root.components();
        rcs.nth(prefix_length - 1);

        let up = (0..rcs.count()).fold(PathBuf::new(), |mut acc, _| {
            acc.push("..");
            acc
        });

        up.join(pcs.as_path()).into()
    }
}

fn version_from_systime(systime: SystemTime) -> u64 {
    systime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// fn replace_source_tmp(source_path: &Path, source_tmp_path: &Path) -> std::io::Result<()> {
//     if source_tmp_path.exists() {
//         std::fs::remove_file(source_tmp_path)?;
//     }

//     match std::fs::hard_link(source_path, source_tmp_path) {
//         Ok(()) => Ok(()),
//         Err(err) => {
//             tracing::debug!("Hard-link to source path '{}' cannot be created at '{}'. {:#}. Fallback to copy instead", source_path.display(), source_tmp_path.display(), err);
//             std::fs::copy(source_path, source_tmp_path)?;
//             Ok(())
//         }
//     }
// }

// fn delete_source_tmp(source_path: &Path, source_tmp_path: &Path) {
//     if source_tmp_path.exists() {
//         if let Err(err) = std::fs::remove_file(source_tmp_path) {
//             tracing::warn!(
//                 "Failed to cleanup source's '{}' copy at '{}'. {:#}",
//                 source_path.display(),
//                 source_tmp_path.display(),
//                 err
//             );
//         }
//     }
// }
