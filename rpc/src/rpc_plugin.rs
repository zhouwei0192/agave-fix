





use std::{ffi::CString, sync::Arc};
use jsonrpc_core::futures::future::join_all;
use libloading::{Library, Symbol};
use solana_account::AccountSharedData;
use solana_client::rpc_config::RpcContextConfig;
use solana_commitment_config::CommitmentConfig;
use solana_message::inner_instruction::InnerInstructions;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::TransactionBinaryEncoding;
use crate::rpc::{decode_and_deserialize, sanitize_transaction};


pub static JRP: once_cell::sync::OnceCell<crate::rpc::JsonRpcRequestProcessor> = once_cell::sync::OnceCell::new();

#[repr(C)]
pub struct AccountCRepr {
    lamports: u64,
    data_ptr: *mut u8,
    data_len: usize,
    owner: [u8; 32], // 固定长度表示 Pubkey
    executable: u8,  // 0 = false, 1 = true
    rent_epoch: u64,
}
// 用于返回结果（成功或错误）
#[repr(C)]
pub struct AccountCResult {
    account: *mut AccountCRepr,
    error_msg: *mut u8, // 如果成功，error_msg = null；失败则存放错误字符串
    // error_len: usize,
}
#[repr(C)]
pub struct PubkeyRepr {
    bytes: [u8; 32],
}
#[repr(C)]
pub struct PubkeyArray {
    ptr: *mut PubkeyRepr,  // 指向数组首元素
    len: usize,            // 数组长度
}
#[repr(C)]
pub struct AccountCArray {
    ptr: *mut *mut AccountCRepr,  // 指向 AccountCRepr* 的数组
    len: usize,                   // 数组长度
    error_msg: *mut u8
}


pub type GetLatestBlockhash = unsafe extern "C" fn() -> [u8; 32];


pub type GetAccount = unsafe extern "C" fn(pk: Pubkey) -> AccountCResult ;


pub type GetMultipleAccount = unsafe extern "C" fn(pks: PubkeyArray) -> AccountCArray;


pub type SimulateTransaction = unsafe extern "C" fn(data: *mut u8,data_len: usize, need_account: bool) -> SimulateResultRepr;

pub type SimulateTransactionV2 = unsafe extern "C" fn(
    data: *mut u8,
    data_len: usize, 
    need_account: bool,
    need_inner_ix: bool
) -> SimulateResultC;

pub type FreeAccount = extern "C" fn(ptr: *mut AccountCRepr) ;
pub type FreeErrMsg = extern "C" fn(ptr: *mut u8) ;

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
        let f5: Symbol<unsafe extern "C" fn(FreeAccount)> = lib.get(b"register_free_account").unwrap();
        f5(free_account);
        let f6: Symbol<unsafe extern "C" fn(FreeErrMsg)> = lib.get(b"register_free_err_msg").unwrap();
        f6(free_err_msg);
        let f7: Symbol<unsafe extern "C" fn(SimulateTransactionV2)> = lib.get(b"register_simulate_transaction_v2").unwrap();
        f7(simulate_transaction_v2);
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
pub extern "C" fn get_account(pk: Pubkey) -> AccountCResult {
    let result = std::panic::catch_unwind(|| {
        // 获取全局 JRP
        let jrp = match JRP.get() {
            Some(j) => j,
            None => return Err("get_account JRP not initialized".to_string()),
        };
        let bank = match jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::processed()),
            min_context_slot: Some(0),
        }) {
            Ok(b) => b,
            Err(e) => return Err(format!("get_account get_bank failed: {}", e)),
        };
        let a = match bank.get_account(&pk) {
            Some(a) => a,
            None => return Err("get_account bank.get_account".to_string()),
        };
        
        // 把 Vec<u8> 的数据转化为 Box<[u8]>，然后把指针分离给 C
        let mut boxed_slice = a.data.to_vec().into_boxed_slice();
        let data_len = boxed_slice.len();
        let data_ptr = boxed_slice.as_mut_ptr();

        // 必须防止 boxed_slice 在离开作用域时被释放，因此使用 Box::into_raw
        let _ = Box::into_raw(boxed_slice);

        let mut owner_bytes = [0u8; 32];
        owner_bytes.copy_from_slice(a.owner.as_ref()); // 视 Pubkey 的可转方法而定

        Ok(AccountCRepr {
            lamports: a.lamports,
            data_ptr,
            data_len,
            owner: owner_bytes,
            executable: if a.executable { 1 } else { 0 },
            rent_epoch: a.rent_epoch,
        })
    });
    match result {
        Ok(Ok(account)) => AccountCResult {
            account: Box::into_raw(Box::new(account)),
            error_msg: std::ptr::null_mut(),
            // error_len: 0,
        },
        Ok(Err(err_str)) => {
            let cstring = CString::new(err_str).unwrap();
            // let len = cstring.as_bytes().len();
            AccountCResult {
                account: std::ptr::null_mut(),
                error_msg: cstring.into_raw() as *mut u8,
                // error_len: len,
            }
        }
        Err(panic_err) => {
            let err_str = panic_to_string(panic_err);
            let cstring = CString::new(err_str).unwrap();
            // let len = cstring.as_bytes().len();
            AccountCResult {
                account: std::ptr::null_mut(),
                error_msg: cstring.into_raw() as *mut u8,
                // error_len: len,
            }
        }
    }
}

fn ffi_to_vec_pubkey(array: PubkeyArray) -> Vec<Pubkey> {
    if array.ptr.is_null() || array.len == 0 {
        return Vec::new();
    }

    // 构造一个临时 slice，安全访问 FFI 数据
    let slice = unsafe { std::slice::from_raw_parts(array.ptr, array.len) };

    // 转成 Vec<Pubkey>
    let pks = slice
        .iter()
        .map(|pk_repr| Pubkey::new_from_array(pk_repr.bytes))
        .collect::<Vec<Pubkey>>();
    free_pubkey_array(array);
    pks
}
pub fn free_pubkey_array(array: PubkeyArray) {
    if array.ptr.is_null() || array.len == 0 {
        return;
    }
    unsafe {
        let _ = Vec::from_raw_parts(array.ptr, array.len, array.len);
    }
}

#[no_mangle]
pub extern "C" fn get_multiple_account(pk_array: PubkeyArray) -> AccountCArray {
    let result = std::panic::catch_unwind(|| {
        let pks = ffi_to_vec_pubkey(pk_array);
        if pks.len() == 0 {
            return Err("pks len = 0".to_string());
        }
        let jrp = match JRP.get() {
            Some(j) => j,
            None => return Err("get_multiple_account JRP not initialized".to_string()),
        };

        let accounts = jrp.runtime.block_on(async {
            let bank = match jrp.get_bank_with_config(RpcContextConfig {
                commitment: Some(CommitmentConfig::processed()),
                min_context_slot: Some(0),
            }) {
                Ok(b) => b,
                Err(e) => return Err(format!("get_multiple_account get_bank failed: {}", e)),
            };

            let mut accounts: Vec<*mut AccountCRepr> = Vec::with_capacity(pks.len());
            let mut tasks = Vec::with_capacity(pks.len());

            for pk in pks {
                let bank = Arc::clone(&bank);
                tasks.push(jrp.runtime.spawn_blocking(move || bank.get_account(&pk)));
            }

            let results = join_all(tasks).await;

            for r in results {
                match r {
                    Ok(Some(a)) => {
                        accounts.push(Box::into_raw(Box::new(to_account_c_repr(a))));
                    }
                    _ => accounts.push(std::ptr::null_mut()),
                }
            }
            Ok(accounts)
        });
        accounts
    });

    match result {
        Ok(Ok(mut accounts_ptrs)) => {
            let (ptr, len) = if accounts_ptrs.is_empty() {
                (std::ptr::null_mut(), 0)
            } else {
                let ptr = accounts_ptrs.as_mut_ptr();
                let len = accounts_ptrs.len();
                std::mem::forget(accounts_ptrs); // 防止 drop
                (ptr, len)
            };
            AccountCArray {
                ptr,
                len,
                error_msg: std::ptr::null_mut(),
            }
        },
        Ok(Err(err_str)) => {
            let cstring = CString::new(err_str).unwrap();
            // let len = cstring.as_bytes().len();
            AccountCArray {
                ptr: std::ptr::null_mut(),
                len: 0,
                error_msg: cstring.into_raw() as *mut u8,
            }
        }
        Err(panic_err) => {
            let err_str = panic_to_string(panic_err);
            let cstring = CString::new(err_str).unwrap();
            // let len = cstring.as_bytes().len();
            AccountCArray {
                ptr: std::ptr::null_mut(),
                len: 0,
                error_msg: cstring.into_raw() as *mut u8,
            }
        }
    }

}

fn to_account_c_repr(a: AccountSharedData) -> AccountCRepr {
    let mut owner_bytes = [0u8; 32];
    owner_bytes.copy_from_slice(a.owner.as_ref());

    let mut boxed_data = a.data.to_vec().into_boxed_slice();
    let data_ptr = boxed_data.as_mut_ptr();
    let data_len = boxed_data.len();
    let _ = Box::into_raw(boxed_data);

    AccountCRepr {
        lamports: a.lamports,
        data_ptr,
        data_len,
        owner: owner_bytes,
        executable: if a.executable { 1 } else { 0 },
        rent_epoch: a.rent_epoch,
    }
}

#[derive(Debug, serde::Serialize)]
pub struct AccountC {
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct SimulateRes {
    // pub error: Option<String>,
    pub logs: Vec<String>,
    pub units_consumed: u64,
    pub post_accounts: Option<Vec<(Pubkey, AccountC)>>,
    pub inner_instructions: Option<Vec<InnerInstructions>>,
}


#[repr(C)]
pub struct SimulateResultC {
    error_ptr: *mut u8,
    error_len: usize,
    data_ptr: *mut u8,
    data_len: usize,
}


#[no_mangle]
pub extern "C" fn simulate_transaction_v2(
    data: *mut u8,
    data_len: usize,
    need_account: bool,
    need_inner_ix: bool
) -> SimulateResultC {
    // 检查 data 指针
    if data.is_null() || data_len == 0 {
        let err = "simulate_transaction_v2: data is null or empty".to_string();
        let (ptr, len) = string_to_leaked_bytes(err);
        return SimulateResultC {
            error_ptr: ptr,
            error_len: len,
            data_ptr: std::ptr::null_mut(),
            data_len: 0,
        };
    }

    // 安全地将输入 bytes 转换为 String
    let data_str = match unsafe { std::str::from_utf8(std::slice::from_raw_parts(data, data_len)) } {
        Ok(s) => s.to_string(),
        Err(_) => {
            let err = "simulate_transaction_v2: invalid UTF-8 in data".to_string();
            let (ptr, len) = string_to_leaked_bytes(err);
            return SimulateResultC {
                error_ptr: ptr,
                error_len: len,
                data_ptr: std::ptr::null_mut(),
                data_len: 0,
            };
        }
    };

    // 捕获 panic
    let result = std::panic::catch_unwind(|| {
        let (_, mut unsanitized_tx) = decode_and_deserialize::<VersionedTransaction>(
            data_str,
            TransactionBinaryEncoding::Base64,
        )
        .map_err(|e| format!("simulate_transaction_v2 decode_and_deserialize error: {:?}", e))?;

        let jrp = match JRP.get() {
            Some(j) => j,
            None => return Err("simulate_transaction_v2 JRP not initialized".to_string()),
        };
        let bank = match jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::processed()),
            min_context_slot: Some(0),
        }) {
            Ok(b) => b,
            Err(e) => return Err(format!("simulate_transaction_v2 jrp.get_bank_with_config: {:?}", e)),
        };
        let recent_blockhash = bank.last_blockhash();
        unsanitized_tx.message.set_recent_blockhash(recent_blockhash);

        let transaction = sanitize_transaction(unsanitized_tx, &*bank, bank.get_reserved_account_keys())
            .map_err(|e| format!("simulate_transaction_v2 error: {:?}", e))?;

        let r = bank.simulate_transaction(&transaction, need_inner_ix);

        // 账户
        let post_accounts_vec = if need_account {
            Some(r.post_simulation_accounts
                .iter()
                .map(|(pk, acc)| {
                    (
                        *pk,
                        AccountC {
                            lamports: acc.lamports,
                            data: acc.data.to_vec(),
                            owner: acc.owner,
                            executable: acc.executable,
                            rent_epoch: acc.rent_epoch,
                        },
                    )
                })
                .collect::<Vec<(Pubkey, AccountC)>>())
        } else {
            None
        };

        let (error_ptr, error_len) = match r.result {
            Ok(_) => (std::ptr::null_mut(), 0),
            Err(e) => string_to_leaked_bytes(e.to_string()),
        };

        let r = SimulateRes {
            // error: todo!(),
            logs: r.logs,
            units_consumed: r.units_consumed,
            post_accounts: post_accounts_vec,
            inner_instructions: r.inner_instructions,
        };

        let (data_ptr, data_len) = vecu8_to_leaked_bytes(bincode::serialize(&r).unwrap());
        // 返回 FFI 结构
        Ok(SimulateResultC {
            error_ptr,
            error_len,
            data_ptr,
            data_len,
        })
    });
    match result {
        Ok(Ok(ffi)) => ffi,
        Ok(Err(err_msg)) => {
            let (ptr, len) = string_to_leaked_bytes(err_msg);
            SimulateResultC {
                error_ptr: ptr,
                error_len: len,
                data_ptr: std::ptr::null_mut(),
                data_len: 0,
            }
        }
        Err(panic_err) => {
            let msg = format!("simulate_transaction_v2 panic: {:?}", panic_err);
            let (ptr, len) = string_to_leaked_bytes(msg);
            SimulateResultC {
                error_ptr: ptr,
                error_len: len,
                data_ptr: std::ptr::null_mut(),
                data_len: 0,
            }
        }
    }
}


/// 表示单个日志（长度安全，不依赖 null）
#[repr(C)]
pub struct LogRepr {
    pub ptr: *mut u8,
    pub len: usize,
}

/// 日志数组
#[repr(C)]
pub struct LogArray {
    pub ptr: *mut LogRepr,
    pub len: usize,
}

/// post account 的 FFI 表示：pubkey + account 指针（account 使用 AccountCRepr 的裸指针）
#[repr(C)]
pub struct PostAccountRepr {
    pub pubkey: [u8; 32],
    pub account: *mut AccountCRepr, // null 表示 None
}

/// post account 数组
#[repr(C)]
pub struct PostAccountArray {
    pub ptr: *mut PostAccountRepr,
    pub len: usize,
}

/// 最外层的 FFI SimulateResult
#[repr(C)]
pub struct SimulateResultRepr {
    pub error_ptr: *mut u8, // error bytes (may be null)
    pub error_len: usize,
    pub logs: LogArray,
    pub units_consumed: u64,
    pub post_accounts: PostAccountArray,
}

/// ---------- 辅助函数：把 String -> 堆分配的 (ptr,len) ----------
fn string_to_leaked_bytes(s: String) -> (*mut u8, usize) {
    let mut boxed = s.into_bytes().into_boxed_slice();
    let len = boxed.len();
    let ptr = boxed.as_mut_ptr();
    // 防止释放，所有权移交给调用方
    std::mem::forget(boxed);
    (ptr, len)
}
fn vecu8_to_leaked_bytes(mut v: Vec<u8>) -> (*mut u8, usize) {
    let len = v.len();
    let ptr = v.as_mut_ptr();
    // 防止 Vec 析构释放内存
    std::mem::forget(v);
    (ptr, len)
}
pub fn free_leaked_bytes(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        unsafe {
            // 重新构造 Box<[u8]>，这样 drop 时 Rust 会自动释放内存
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(ptr, len));
        }
    }
}
/// ---------- 把 Vec<String> -> LogArray ----------
fn logs_to_logarray(logs: Vec<String>) -> LogArray {
    if logs.is_empty() {
        return LogArray { ptr: std::ptr::null_mut(), len: 0 };
    }

    // 构造 Vec<LogRepr>
    let mut reprs: Vec<LogRepr> = logs.into_iter().map(|s| {
        let (ptr, len) = string_to_leaked_bytes(s);
        LogRepr { ptr, len }
    }).collect();

    let len = reprs.len();
    let ptr = reprs.as_mut_ptr();
    std::mem::forget(reprs);
    LogArray { ptr, len }
}

/// ---------- 把 Vec<(Pubkey, AccountCRepr)> -> PostAccountArray ----------
/// 假设 AccountCRepr 是普通 Rust struct（包含 data_ptr,data_len 等），
// 这里我们把每个 AccountCRepr 放到堆上并返回裸指针（C 侧或 free 函数负责释放）
fn post_accounts_to_array(post_vec: Option<Vec<(Pubkey, AccountCRepr)>>) -> PostAccountArray {
    let post = match post_vec{
        Some(p) => p,
        None => return PostAccountArray { ptr: std::ptr::null_mut(), len: 0 },
    };

    let mut reprs: Vec<PostAccountRepr> = post.into_iter().map(|(pk, acc)| {
        // convert acc -> heap pointer
        let acc_ptr = Box::into_raw(Box::new(acc));
        let mut pubkey_bytes = [0u8; 32];
        pubkey_bytes.copy_from_slice(pk.as_ref());
        PostAccountRepr { pubkey: pubkey_bytes, account: acc_ptr }
    }).collect();

    let len = reprs.len();
    let ptr = reprs.as_mut_ptr();
    std::mem::forget(reprs);
    PostAccountArray { ptr, len }
}

#[no_mangle]
pub extern "C" fn simulate_transaction(
    data: *mut u8,
    data_len: usize,
    need_account: bool,
) -> SimulateResultRepr {
    // 默认空返回值
    let empty_result = || SimulateResultRepr {
        error_ptr: std::ptr::null_mut(),
        error_len: 0,
        logs: LogArray {
            ptr: std::ptr::null_mut(),
            len: 0,
        },
        units_consumed: 0,
        post_accounts: PostAccountArray {
            ptr: std::ptr::null_mut(),
            len: 0,
        },
    };

    // 检查 data 指针
    if data.is_null() || data_len == 0 {
        let err = "simulate_transaction: data is null or empty".to_string();
        let (ptr, len) = string_to_leaked_bytes(err);
        return SimulateResultRepr {
            error_ptr: ptr,
            error_len: len,
            ..empty_result()
        };
    }

    // 安全地将输入 bytes 转换为 String
    let data_str = match unsafe { std::str::from_utf8(std::slice::from_raw_parts(data, data_len)) } {
        Ok(s) => s.to_string(),
        Err(_) => {
            let err = "simulate_transaction: invalid UTF-8 in data".to_string();
            let (ptr, len) = string_to_leaked_bytes(err);
            return SimulateResultRepr {
                error_ptr: ptr,
                error_len: len,
                ..empty_result()
            };
        }
    };

    // 捕获 panic
    let result = std::panic::catch_unwind(|| {
        let (_, mut unsanitized_tx) = decode_and_deserialize::<VersionedTransaction>(
            data_str,
            TransactionBinaryEncoding::Base64,
        )
        .map_err(|e| format!("decode_and_deserialize error: {:?}", e))?;

        let jrp = match JRP.get() {
            Some(j) => j,
            None => return Err("simulate_transaction JRP not initialized".to_string()),
        };
        let bank = match jrp.get_bank_with_config(RpcContextConfig {
            commitment: Some(CommitmentConfig::processed()),
            min_context_slot: Some(0),
        }) {
            Ok(b) => b,
            Err(e) => return Err(format!("simulate_transaction jrp.get_bank_with_config: {:?}", e)),
        };
        let recent_blockhash = bank.last_blockhash();
        unsanitized_tx.message.set_recent_blockhash(recent_blockhash);

        let transaction = sanitize_transaction(unsanitized_tx, &*bank, bank.get_reserved_account_keys())
            .map_err(|e| format!("sanitize_transaction error: {:?}", e))?;

        let r = bank.simulate_transaction(&transaction, false);

        // 账户
        let post_accounts_vec = if need_account {
            Some(r.post_simulation_accounts
                .iter()
                .map(|(pk, acc)| {
                    (
                        *pk,
                        AccountCRepr {
                            lamports: acc.lamports,
                            data_ptr: {
                                let mut boxed = acc.data.clone().to_vec().into_boxed_slice();
                                let ptr = boxed.as_mut_ptr();
                                std::mem::forget(boxed);
                                ptr
                            },
                            data_len: acc.data.len(),
                            owner: acc.owner.to_bytes(),
                            executable: acc.executable as u8,
                            rent_epoch: acc.rent_epoch,
                        },
                    )
                })
                .collect::<Vec<(Pubkey, AccountCRepr)>>())
        } else {
            None
        };

        // let post_accounts = post_accounts_vec.unwrap_or_default();
        let (error_ptr, error_len) = match r.result {
            Ok(_) => (std::ptr::null_mut(), 0),
            Err(e) => string_to_leaked_bytes(e.to_string()),
        };
        let units_consumed = r.units_consumed;
        let logs = logs_to_logarray(r.logs);
        let post_accounts = post_accounts_to_array(post_accounts_vec);
        // 返回 FFI 结构
        Ok(SimulateResultRepr {
            error_ptr,
            error_len,
            logs,
            units_consumed,
            post_accounts,
        })
    });
    match result {
        Ok(Ok(ffi)) => ffi,
        Ok(Err(err_msg)) => {
            let (ptr, len) = string_to_leaked_bytes(err_msg);
            SimulateResultRepr {
                error_ptr: ptr,
                error_len: len,
                ..empty_result()
            }
        }
        Err(panic_err) => {
            let msg = format!("simulate_transaction panic: {:?}", panic_err);
            let (ptr, len) = string_to_leaked_bytes(msg);
            SimulateResultRepr {
                error_ptr: ptr,
                error_len: len,
                ..empty_result()
            }
        }
    }
}


#[no_mangle]
pub extern "C" fn free_err_msg(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        // 安全地将 *mut u8 转为 *mut c_char
        let c_ptr = ptr as *mut std::os::raw::c_char;
        // 从裸指针恢复 CString 并释放
        let _ = CString::from_raw(c_ptr);
    }
}

#[no_mangle]
pub extern "C" fn free_account(ptr: *mut AccountCRepr) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        // 取回 Box 并释放主结构体
        let account = Box::from_raw(ptr);

        // 同时释放 data 数据
        if !account.data_ptr.is_null() && account.data_len > 0 {
            let slice = std::slice::from_raw_parts_mut(account.data_ptr, account.data_len);
            let _ = Box::from_raw(slice as *mut [u8]);
        }
    }
}

fn panic_to_string(panic_err: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic_err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_err.downcast_ref::<String>() {
        s.clone()
    } else {
        "Unknown panic occurred".to_string()
    }
}