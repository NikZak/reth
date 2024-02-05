//! Database debugging tool

use crate::{
    args::{
        utils::{chain_help, genesis_value_parser, SUPPORTED_CHAINS},
        DatabaseArgs, StageEnum,
    },
    dirs::{DataDirPath, MaybePlatformPath},
    init::{insert_genesis_header, insert_genesis_state},
    utils::DbTool,
};
use clap::Parser;
use reth_db::{
    database::Database, mdbx::DatabaseArguments, open_db, snapshot::iter_snapshots, tables,
    transaction::DbTxMut, DatabaseEnv,
};
use reth_primitives::{fs, snapshot::find_fixed_range, stage::StageId, ChainSpec, SnapshotSegment};
use reth_provider::ProviderFactory;
use std::sync::Arc;
use tracing::info;

/// `reth drop-stage` command
#[derive(Debug, Parser)]
pub struct Command {
    /// The path to the data dir for all reth files and subdirectories.
    ///
    /// Defaults to the OS-specific data directory:
    ///
    /// - Linux: `$XDG_DATA_HOME/reth/` or `$HOME/.local/share/reth/`
    /// - Windows: `{FOLDERID_RoamingAppData}/reth/`
    /// - macOS: `$HOME/Library/Application Support/reth/`
    #[arg(long, value_name = "DATA_DIR", verbatim_doc_comment, default_value_t)]
    datadir: MaybePlatformPath<DataDirPath>,

    /// The chain this node is running.
    ///
    /// Possible values are either a built-in chain or the path to a chain specification file.
    #[arg(
        long,
        value_name = "CHAIN_OR_PATH",
        long_help = chain_help(),
        default_value = SUPPORTED_CHAINS[0],
        value_parser = genesis_value_parser
    )]
    chain: Arc<ChainSpec>,

    #[clap(flatten)]
    db: DatabaseArgs,

    stage: StageEnum,
}

impl Command {
    /// Execute `db` command
    pub async fn execute(self) -> eyre::Result<()> {
        // add network name to data dir
        let data_dir = self.datadir.unwrap_or_chain_default(self.chain.chain);
        let db_path = data_dir.db_path();
        fs::create_dir_all(&db_path)?;

        let db =
            open_db(db_path.as_ref(), DatabaseArguments::default().log_level(self.db.log_level))?;
        let provider_factory =
            ProviderFactory::new(db, self.chain.clone(), data_dir.snapshots_path())?;
        let snapshot_provider = provider_factory.snapshot_provider();

        let tool = DbTool::new(provider_factory, self.chain.clone())?;

        tool.provider_factory.db_ref().update(|tx| {
            match self.stage {
                StageEnum::Bodies => {
                    tx.clear::<tables::BlockBodyIndices>()?;
                    tx.clear::<tables::Transactions>()?;
                    tx.clear::<tables::TransactionBlock>()?;
                    tx.clear::<tables::BlockOmmers>()?;
                    tx.clear::<tables::BlockWithdrawals>()?;
                    tx.put::<tables::SyncStage>(StageId::Bodies.to_string(), Default::default())?;
                    insert_genesis_header::<DatabaseEnv>(tx, snapshot_provider, self.chain)?;
                }
                StageEnum::Senders => {
                    tx.clear::<tables::TxSenders>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::SenderRecovery.to_string(),
                        Default::default(),
                    )?;
                }
                StageEnum::Execution => {
                    tx.clear::<tables::PlainAccountState>()?;
                    tx.clear::<tables::PlainStorageState>()?;
                    tx.clear::<tables::AccountChangeSet>()?;
                    tx.clear::<tables::StorageChangeSet>()?;
                    tx.clear::<tables::Bytecodes>()?;
                    tx.clear::<tables::Receipts>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::Execution.to_string(),
                        Default::default(),
                    )?;
                    insert_genesis_state::<DatabaseEnv>(tx, self.chain.genesis())?;
                }
                StageEnum::AccountHashing => {
                    tx.clear::<tables::HashedAccount>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::AccountHashing.to_string(),
                        Default::default(),
                    )?;
                }
                StageEnum::StorageHashing => {
                    tx.clear::<tables::HashedStorage>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::StorageHashing.to_string(),
                        Default::default(),
                    )?;
                }
                StageEnum::Hashing => {
                    // Clear hashed accounts
                    tx.clear::<tables::HashedAccount>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::AccountHashing.to_string(),
                        Default::default(),
                    )?;

                    // Clear hashed storages
                    tx.clear::<tables::HashedStorage>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::StorageHashing.to_string(),
                        Default::default(),
                    )?;
                }
                StageEnum::Merkle => {
                    tx.clear::<tables::AccountsTrie>()?;
                    tx.clear::<tables::StoragesTrie>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::MerkleExecute.to_string(),
                        Default::default(),
                    )?;
                    tx.put::<tables::SyncStage>(
                        StageId::MerkleUnwind.to_string(),
                        Default::default(),
                    )?;
                    tx.delete::<tables::SyncStageProgress>(
                        StageId::MerkleExecute.to_string(),
                        None,
                    )?;
                }
                StageEnum::AccountHistory | StageEnum::StorageHistory => {
                    tx.clear::<tables::AccountHistory>()?;
                    tx.clear::<tables::StorageHistory>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::IndexAccountHistory.to_string(),
                        Default::default(),
                    )?;
                    tx.put::<tables::SyncStage>(
                        StageId::IndexStorageHistory.to_string(),
                        Default::default(),
                    )?;
                }
                StageEnum::TotalDifficulty => {
                    tx.clear::<tables::HeaderTD>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::TotalDifficulty.to_string(),
                        Default::default(),
                    )?;
                    insert_genesis_header::<DatabaseEnv>(tx, snapshot_provider, self.chain)?;
                }
                StageEnum::TxLookup => {
                    tx.clear::<tables::TxHashNumber>()?;
                    tx.put::<tables::SyncStage>(
                        StageId::TransactionLookup.to_string(),
                        Default::default(),
                    )?;
                    insert_genesis_header::<DatabaseEnv>(tx, snapshot_provider, self.chain)?;
                }
                _ => {
                    info!("Nothing to do for stage {:?}", self.stage);
                    return Ok(())
                }
            }

            tx.put::<tables::SyncStage>(StageId::Finish.to_string(), Default::default())?;

            Ok::<_, eyre::Error>(())
        })??;

        let snapshot_segment = match self.stage {
            StageEnum::Headers => Some(SnapshotSegment::Headers),
            StageEnum::Bodies => Some(SnapshotSegment::Transactions),
            StageEnum::Execution => Some(SnapshotSegment::Receipts),
            _ => None,
        };

        if let Some(snapshot_segment) = snapshot_segment {
            let snapshot_provider = tool.provider_factory.snapshot_provider();
            let snapshots = iter_snapshots(snapshot_provider.directory())?;
            if let Some(segment_snapshots) = snapshots.get(&snapshot_segment) {
                for (block_range, _) in segment_snapshots {
                    snapshot_provider
                        .delete_jar(snapshot_segment, find_fixed_range(*block_range.start()))?;
                }
            }
        }

        Ok(())
    }
}
