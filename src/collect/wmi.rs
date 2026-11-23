//! Minimal WMI queries over COM.
//!
//! One function, [`query`], runs a WQL query and returns every row as a map
//! from property name to a plain value. COM is initialised for the calling
//! thread for the duration of the call and every interface is released
//! before it returns. WMI is slow (tens to hundreds of milliseconds per
//! query): call it from a helper thread and never on every tick.

use std::collections::HashMap;
use windows::core::{BSTR, PCWSTR, VARIANT};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_NONSYSTEM_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
};

/// `RPC_C_AUTHN_WINNT` / `RPC_C_AUTHZ_NONE`.
const AUTHN_WINNT: u32 = 10;
const AUTHZ_NONE: u32 = 0;
/// Longest wait for the next row, in milliseconds.
const ROW_TIMEOUT_MS: i32 = 5000;
/// `IEnumWbemClassObject::Next` gave up waiting (a success code, not an error).
const WBEM_S_TIMEDOUT: i32 = 0x0004_0004;
/// A query never returns more rows than this to the caller.
const MAX_ROWS: usize = 4096;
/// Properties read per row at most.
const MAX_PROPERTIES: usize = 512;

/// One property value, reduced to what the collectors need.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The property exists but has no value.
    Null,
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    /// The value as an integer. WMI delivers 64-bit numbers as text, so
    /// numeric text counts too.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Float(v) if v.is_finite() && v.abs() < 9.0e18 => Some(*v as i64),
            Value::Bool(v) => Some(i64::from(*v)),
            Value::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }

    /// The value as a floating-point number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(v) => Some(*v as f64),
            Value::Float(v) if v.is_finite() => Some(*v),
            Value::Text(t) => t.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
            _ => None,
        }
    }

    /// The text of a text value, trimmed. Numbers are not converted.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t.trim()),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// One result row: property name to value.
pub type Row = HashMap<String, Value>;

/// True when an error text from [`query`] means "not allowed for this user"
/// (the WMI and the Win32 "access denied" codes).
pub fn is_access_denied(error: &str) -> bool {
    error.contains("0x80041003") || error.contains("0x80070005")
}

/// Balances a successful `CoInitializeEx` when dropped.
struct ComGuard {
    uninitialize: bool,
}

impl ComGuard {
    fn new() -> Result<ComGuard, String> {
        // SAFETY: plain initialisation of COM for this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            // S_OK and S_FALSE both need a matching uninitialise.
            Ok(ComGuard { uninitialize: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // The thread already uses COM in another mode: use it as it is.
            Ok(ComGuard {
                uninitialize: false,
            })
        } else {
            Err(format!("COM is not available (0x{:08X})", hr.0 as u32))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: balances the successful CoInitializeEx of `new` on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `wql` in `namespace` (for example `root\cimv2`) and returns the rows.
/// Errors are short English texts that include the HRESULT.
pub fn query(namespace: &str, wql: &str) -> Result<Vec<Row>, String> {
    // Declared first, so it is dropped last: every interface below is
    // released before COM is uninitialised.
    let _com = ComGuard::new()?;
    let text =
        |what: &str, e: windows::core::Error| format!("{what} (0x{:08X})", e.code().0 as u32);

    // SAFETY: standard in-process activation of the WMI locator.
    let locator: IWbemLocator =
        unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| text("WMI is not available", e))?;
    // SAFETY: all strings are valid BSTRs for the duration of the call.
    let services = unsafe {
        locator.ConnectServer(
            &BSTR::from(namespace),
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )
    }
    .map_err(|e| text("could not open the WMI namespace", e))?;
    // Let the service act for this user on this connection only; the
    // process-wide security settings are not touched.
    // SAFETY: `services` is a valid proxy; no server principal name is given.
    unsafe {
        CoSetProxyBlanket(
            &services,
            AUTHN_WINNT,
            AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    }
    .map_err(|e| text("could not set up the WMI connection", e))?;

    let flags = WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0);
    // SAFETY: valid BSTRs; the enumerator is released when dropped.
    let rows = unsafe { services.ExecQuery(&BSTR::from("WQL"), &BSTR::from(wql), flags, None) }
        .map_err(|e| text("the WMI query failed", e))?;

    let mut out = Vec::new();
    while out.len() < MAX_ROWS {
        let mut slot: [Option<IWbemClassObject>; 1] = [None];
        let mut returned = 0u32;
        // SAFETY: `slot` has room for the one object asked for.
        let hr = unsafe { rows.Next(ROW_TIMEOUT_MS, &mut slot, &mut returned) };
        if hr.is_err() {
            // Semi-synchronous queries report their failure on the first fetch.
            if out.is_empty() {
                return Err(format!("the WMI query failed (0x{:08X})", hr.0 as u32));
            }
            break;
        }
        // "Timed out" is a success code: without this check a slow answer
        // (a busy computer) would be taken for "no rows".
        if hr.0 == WBEM_S_TIMEDOUT {
            if out.is_empty() {
                return Err("WMI did not answer in time".to_string());
            }
            break;
        }
        let Some(object) = slot[0].take().filter(|_| returned == 1) else {
            break;
        };
        out.push(read_row(&object));
    }
    Ok(out)
}

/// Reads every non-system property of one object.
fn read_row(object: &IWbemClassObject) -> Row {
    let mut row = Row::new();
    // SAFETY: starts an enumeration that is ended below.
    if unsafe { object.BeginEnumeration(WBEM_FLAG_NONSYSTEM_ONLY.0) }.is_err() {
        return row;
    }
    for _ in 0..MAX_PROPERTIES {
        let mut name = BSTR::new();
        let mut value = VARIANT::new();
        // SAFETY: both out-parameters are valid and own what they receive
        // (they free it when dropped); the optional outputs are not requested.
        let next = unsafe {
            object.Next(
                0,
                &mut name,
                &mut value,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // The end of the list is a success code with an empty name.
        if next.is_err() || name.is_empty() {
            break;
        }
        row.insert(name.to_string(), convert(&value));
    }
    // SAFETY: ends the enumeration started above.
    let _ = unsafe { object.EndEnumeration() };
    row
}

/// Variant type tags used by WMI.
const VT_EMPTY: u16 = 0;
const VT_NULL: u16 = 1;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_BSTR: u16 = 8;
const VT_BOOL: u16 = 11;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;

fn convert(variant: &VARIANT) -> Value {
    let raw = variant.as_raw();
    // SAFETY: every VARIANT starts with its type tag.
    let tag = unsafe { raw.Anonymous.Anonymous.vt };
    // SAFETY (for each read below): the union member that is read is the one
    // the type tag names; all of them are plain numbers.
    let number = unsafe { &raw.Anonymous.Anonymous.Anonymous };
    match tag {
        VT_EMPTY | VT_NULL => Value::Null,
        VT_I2 => Value::Int(i64::from(unsafe { number.iVal })),
        VT_I4 | VT_INT => Value::Int(i64::from(unsafe { number.lVal })),
        VT_I1 => Value::Int(i64::from(unsafe { number.cVal })),
        VT_UI1 => Value::Int(i64::from(unsafe { number.bVal })),
        VT_UI2 => Value::Int(i64::from(unsafe { number.uiVal })),
        VT_UI4 | VT_UINT => Value::Int(i64::from(unsafe { number.ulVal })),
        VT_I8 => Value::Int(unsafe { number.llVal }),
        VT_UI8 => Value::Int(i64::try_from(unsafe { number.ullVal }).unwrap_or(i64::MAX)),
        VT_R4 => Value::Float(f64::from(unsafe { number.fltVal })),
        VT_R8 => Value::Float(unsafe { number.dblVal }),
        VT_BOOL => Value::Bool(unsafe { number.boolVal } != 0),
        VT_BSTR => match BSTR::try_from(variant) {
            Ok(text) => Value::Text(text.to_string()),
            Err(_) => Value::Null,
        },
        // Arrays, objects and dates in other encodings are not needed here.
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_accessors() {
        assert_eq!(Value::Int(42).as_i64(), Some(42));
        assert_eq!(Value::Text(" 3010 ".into()).as_i64(), Some(3010));
        assert_eq!(Value::Text("18446744073709551615".into()).as_i64(), None);
        assert_eq!(Value::Text("abc".into()).as_i64(), None);
        assert_eq!(Value::Text(String::new()).as_i64(), None);
        assert_eq!(Value::Float(12.9).as_i64(), Some(12));
        assert_eq!(Value::Float(f64::NAN).as_i64(), None);
        assert_eq!(Value::Float(1e300).as_i64(), None);
        assert_eq!(Value::Bool(true).as_i64(), Some(1));
        assert_eq!(Value::Null.as_i64(), None);

        assert_eq!(Value::Int(-3).as_f64(), Some(-3.0));
        assert_eq!(Value::Text("2.5".into()).as_f64(), Some(2.5));
        assert_eq!(Value::Text("inf".into()).as_f64(), None);
        assert_eq!(Value::Text("NaN".into()).as_f64(), None);
        assert_eq!(Value::Float(f64::INFINITY).as_f64(), None);
        assert_eq!(Value::Null.as_f64(), None);
        assert_eq!(Value::Bool(true).as_f64(), None);

        assert_eq!(Value::Text("  name ".into()).as_text(), Some("name"));
        assert_eq!(Value::Int(1).as_text(), None);
        assert!(Value::Null.is_null());
        assert!(!Value::Int(0).is_null());
    }

    #[test]
    fn variants_convert() {
        assert_eq!(convert(&VARIANT::new()), Value::Null);
        assert_eq!(convert(&VARIANT::from(-7i32)), Value::Int(-7));
        assert_eq!(convert(&VARIANT::from(7u32)), Value::Int(7));
        assert_eq!(convert(&VARIANT::from(1i64 << 40)), Value::Int(1 << 40));
        assert_eq!(convert(&VARIANT::from(u64::MAX)), Value::Int(i64::MAX));
        assert_eq!(convert(&VARIANT::from(2.5f64)), Value::Float(2.5));
        assert_eq!(convert(&VARIANT::from(true)), Value::Bool(true));
        assert_eq!(convert(&VARIANT::from(false)), Value::Bool(false));
        assert_eq!(convert(&VARIANT::from("zone")), Value::Text("zone".into()));
        assert_eq!(convert(&VARIANT::from("")), Value::Text(String::new()));
    }

    #[test]
    fn access_denied_is_recognised() {
        assert!(is_access_denied("the WMI query failed (0x80041003)"));
        assert!(is_access_denied(
            "could not open the WMI namespace (0x80070005)"
        ));
        assert!(!is_access_denied("the WMI query failed (0x80041010)"));
        assert!(!is_access_denied(""));
    }

    #[test]
    fn live_query_smoke() {
        let started = std::time::Instant::now();
        let rows = query(
            r"root\cimv2",
            "SELECT NumberOfLogicalProcessors FROM Win32_ComputerSystem",
        );
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        match rows {
            Ok(rows) => {
                assert_eq!(rows.len(), 1);
                let count = rows[0]
                    .get("NumberOfLogicalProcessors")
                    .and_then(Value::as_i64);
                assert!(count.is_some_and(|n| n >= 1));
                println!("wmi: query ok, rows {}, {ms:.0} ms", rows.len());
            }
            Err(error) => println!("wmi: not available ({error})"),
        }
        // Failures come back as errors, not panics, and leave COM balanced.
        assert!(query(r"root\no_such_namespace_here", "SELECT * FROM Nothing").is_err());
        assert!(query(r"root\cimv2", "SELECT * FROM No_Such_Class_Here").is_err());
        assert!(query(r"root\cimv2", "this is not a query").is_err());
        // And a second good query still works afterwards.
        let again = query(r"root\cimv2", "SELECT Name FROM Win32_Processor");
        assert!(again.is_ok_and(|rows| !rows.is_empty()));
    }
}
