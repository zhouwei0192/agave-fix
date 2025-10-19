





use std::{sync::Arc, vec};
use jsonrpc_core::futures::future::join_all;
use libloading::{Library, Symbol};
use solana_client::rpc_config::RpcContextConfig;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::TransactionBinaryEncoding;
use crate::rpc::{decode_and_deserialize, sanitize_transaction};


pub static JRP: once_cell::sync::OnceCell<crate::rpc::JsonRpcRequestProcessor> = once_cell::sync::OnceCell::new();


#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct AccountC{
    // lamports in the account
    pub lamports: u64,
    // data held in this account
    pub data: Vec<u8>,
    // the program that owns this account. If executable, the program that loads this account.
    pub owner: Pubkey,
    // this account's data contains a loaded program (and is now read-only)
    pub executable: bool,
    // the epoch at which this account will next owe rent
    pub rent_epoch: u64,
}

#[allow(improper_ctypes_definitions)]
pub type GetLatestBlockhash = unsafe extern "C" fn() -> [u8; 32];

#[allow(improper_ctypes_definitions)]
pub type GetAccount = unsafe extern "C" fn(pk: Pubkey) -> Option<AccountC> ;

#[allow(improper_ctypes_definitions)]
pub type GetMultipleAccount = unsafe extern "C" fn(pks: Vec<Pubkey>) -> Vec<Option<AccountC>>;

#[allow(improper_ctypes_definitions)]
pub type SimulateTransaction = unsafe extern "C" fn(data: String, need_account: bool) -> SimulateResult;


pub fn load_fn(lib: &Library) {
    unsafe {
        let f: Symbol<unsafe extern "C" fn(GetAccount)> = lib.get(b"register_get_account").unwrap();
        f(get_account);
        let f2: Symbol<unsafe extern "C" fn(GetMultipleAccount)> = lib.get(b"register_get_multiple_account").unwrap();
        f2(get_multiple_account);
        let f3: Symbol<unsafe extern "C" fn(SimulateTransaction)> = lib.get(b"register_simulate_transaction").unwrap();
        f3(simulate_transaction);
        let f4: Symbol<unsafe extern "C" fn(GetLatestBlockhash)> = lib.get(b"register_get_latest_blockhash").unwrap();
        f4(get_latest_blockhash);
    }
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn get_latest_blockhash() -> [u8; 32] {
    let result = std::panic::catch_unwind(|| {
        let jrp = JRP.get().unwrap();
        let bank = jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            min_context_slot: Some(0),
        }).unwrap();
        bank.last_blockhash().to_bytes()
    });

    match result {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("get_latest_blockhash panic: {:?}", err);
            return [0u8; 32]
        }
    }
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn get_account(pk: Pubkey) -> Option<AccountC> {
    let result = std::panic::catch_unwind(|| {
        let jrp = JRP.get().unwrap();
        let bank = jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            min_context_slot: Some(0),
        }).unwrap();
        let a = bank.get_account(&pk).unwrap();
        Some(AccountC {
            lamports:a.lamports,
            data: a.data.to_vec(),
            owner: a.owner,
            executable: a.executable,
            rent_epoch: a.rent_epoch,
        })
    });

    match result {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("get_account panic: {:?}", err);
            return None
        }
    }
}


#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn get_multiple_account(pks: Vec<Pubkey>) -> Vec<Option<AccountC>> {
    let result = std::panic::catch_unwind(|| {
        let jrp = JRP.get().unwrap();
        let accounts = jrp.runtime.block_on( async {
            let bank = jrp.get_bank_with_config(RpcContextConfig {
                commitment: Some(CommitmentConfig::confirmed()),
                min_context_slot: Some(0),
            }).unwrap();
        
            let mut accounts = Vec::with_capacity(pks.len());
            let mut tasks = Vec::with_capacity(pks.len());
        
            for pk in pks {
                let bank = Arc::clone(&bank);
                tasks.push(
                    jrp.runtime.spawn_blocking(move || {
                        bank.get_account(&pk)
                    })
                );
            }
            let results = join_all(tasks).await;
            for r in results {
                if let Ok(a) = r {
                    let a= a.unwrap();
                    accounts.push(Some(AccountC {
                        lamports:a.lamports,
                        data: a.data.to_vec(),
                        owner: a.owner,
                        executable: a.executable,
                        rent_epoch: a.rent_epoch,
                    }));
                }
            }
            accounts
        });
        accounts
    });

    match result {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("get_multiple_account panic: {:?}", err);
            return vec![]
        }
    }

}


#[repr(C)]
#[derive(Debug)]
pub struct SimulateResult {
    pub error: Option<String>,
    pub logs: Vec<String>,
    pub units_consumed: u64,
    pub post_accounts: Option<Vec<(Pubkey, AccountC)>>
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn simulate_transaction(
    data: String,
    need_account: bool
) -> SimulateResult {
    let result = std::panic::catch_unwind(|| {
        let (_, unsanitized_tx) =
            decode_and_deserialize::<VersionedTransaction>(data, TransactionBinaryEncoding::Base64).unwrap();

        let jrp = JRP.get().unwrap();

        let bank = &*jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::processed()),
            min_context_slot: Some(0),
        }).unwrap();

        let transaction =
            sanitize_transaction(unsanitized_tx, bank, bank.get_reserved_account_keys()).unwrap();

        let r = bank.simulate_transaction(&transaction, false);
        let post_accounts = if need_account {
            Some(
                r.post_simulation_accounts
                    .iter()
                    .map(|v| {
                        (
                            v.0, 
                            AccountC {
                                lamports:v.1.lamports,
                                data: v.1.data.to_vec(),
                                owner: v.1.owner,
                                executable: v.1.executable,
                                rent_epoch: v.1.rent_epoch,
                            }
                        )
                    })
                    .collect::<Vec<(Pubkey, AccountC)>>()
            )
        } else {
            None
        };
        
        SimulateResult {
            error: r.result.err().map(|e| e.to_string()),
            logs: r.logs,
            units_consumed: r.units_consumed,
            post_accounts: post_accounts
        }
    });

    match result {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("simulate_transaction panic: {:?}", err);
            return SimulateResult {
                error: Some(format!("simulate_transaction panic: {:?}", err)),
                logs: vec![],
                units_consumed: 0,
                post_accounts: None
            }
        }
    }
    
}