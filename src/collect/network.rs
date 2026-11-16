//! Network adapters: addresses and live transfer rates.
//!
//! The adapter list with names and addresses comes from
//! `GetAdaptersAddresses` and is refreshed every few seconds; it does not
//! contain the per-filter duplicates of an adapter that the raw interface
//! table has. The byte counters, link speed and state of each listed adapter
//! are read for every sample with `GetIfEntry2`, on a helper thread so that a
//! slow network driver cannot stall the caller.
//!
//! An adapter counts as "Virtual" when Windows says it has no hardware
//! behind it (virtual switches, VPN and tunnel adapters). The traffic of
//! those passes through a physical adapter as well, so the totals of the
//! program leave them out.

use super::pdh::{le_u16, rate};
use crate::model::NetSample;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetIfEntry2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
};
use windows::Win32::Networking::WinSock::SOCKET_ADDRESS;

/// How often names and addresses are re-read.
const ADDRESS_PERIOD: Duration = Duration::from_secs(10);
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const IF_OPER_STATUS_UP: i32 = 1;

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_LOOPBACK: u32 = 24;
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;
const IF_TYPE_WWAN: u32 = 243;
const IF_TYPE_WWAN2: u32 = 244;

const MEDIUM_WIRELESS_LAN: u32 = 1;
const MEDIUM_WIRELESS_WAN: u32 = 8;
const MEDIUM_NATIVE_802_11: u32 = 9;
const MEDIUM_BLUETOOTH: u32 = 10;

// ----------------------------------------------------------------------------------
// Pure helpers
// ----------------------------------------------------------------------------------

/// Words in a hardware description that mark an adapter without hardware.
/// Only used when Windows does not answer the question itself.
const VIRTUAL_WORDS: [&str; 8] = [
    "virtual",
    "vpn",
    "tunnel",
    "tap-",
    "tap adapter",
    "pseudo",
    "miniport",
    "vethernet",
];

/// The kind of an adapter: "Ethernet", "Wi-Fi", "Cellular", "Bluetooth",
/// "Virtual", "Loopback" or "Other".
///
/// `hardware` is the answer of Windows to "is there a device behind this
/// interface" (`None` when it could not be asked). `medium` is the physical
/// medium number of NDIS.
pub fn classify(
    if_type: u32,
    hardware: Option<bool>,
    medium: u32,
    description: &str,
) -> &'static str {
    let lower = description.to_ascii_lowercase();
    if if_type == IF_TYPE_LOOPBACK || lower.contains("loopback") {
        return "Loopback";
    }
    if matches!(if_type, IF_TYPE_TUNNEL | IF_TYPE_PPP | IF_TYPE_PROP_VIRTUAL) {
        return "Virtual";
    }
    let virtual_adapter = match hardware {
        Some(real) => !real,
        None => VIRTUAL_WORDS.iter().any(|word| lower.contains(word)),
    };
    if virtual_adapter {
        return "Virtual";
    }
    if medium == MEDIUM_BLUETOOTH || lower.contains("bluetooth") {
        "Bluetooth"
    } else if if_type == IF_TYPE_WIFI
        || medium == MEDIUM_NATIVE_802_11
        || medium == MEDIUM_WIRELESS_LAN
    {
        "Wi-Fi"
    } else if matches!(if_type, IF_TYPE_WWAN | IF_TYPE_WWAN2) || medium == MEDIUM_WIRELESS_WAN {
        "Cellular"
    } else if if_type == IF_TYPE_ETHERNET {
        "Ethernet"
    } else {
        "Other"
    }
}

/// True for the kinds that are a real connection of this machine.
pub fn is_physical(kind: &str) -> bool {
    kind != "Virtual" && kind != "Loopback"
}

/// Whether an adapter is worth showing: connected, or a physical adapter
/// (which is shown as disconnected).
pub fn worth_showing(kind: &str, up: bool) -> bool {
    up || is_physical(kind)
}

/// Position in the list: connected physical adapters first.
fn sort_rank(kind: &str, up: bool) -> (u8, u8) {
    let group = match (is_physical(kind), up) {
        (true, true) => 0,
        (true, false) => 1,
        (false, _) => 2,
    };
    let order = match kind {
        "Ethernet" => 0,
        "Wi-Fi" => 1,
        "Cellular" => 2,
        "Bluetooth" => 3,
        "Other" => 4,
        "Virtual" => 5,
        _ => 6,
    };
    (group, order)
}

/// "AA-BB-CC-DD-EE-FF"; empty for no address or an all-zero one.
pub fn format_mac(bytes: &[u8]) -> String {
    if bytes.is_empty() || bytes.len() > 32 || bytes.iter().all(|&b| b == 0) {
        return String::new();
    }
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// An address as text and whether it is IPv6, from the bytes of a socket
/// address structure. Unknown families and short buffers give `None`.
pub fn format_sockaddr(bytes: &[u8]) -> Option<(String, bool)> {
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;
    match le_u16(bytes, 0)? {
        AF_INET => {
            let a: [u8; 4] = bytes.get(4..8)?.try_into().ok()?;
            Some((Ipv4Addr::from(a).to_string(), false))
        }
        AF_INET6 => {
            let a: [u8; 16] = bytes.get(8..24)?.try_into().ok()?;
            Some((Ipv6Addr::from(a).to_string(), true))
        }
        _ => None,
    }
}

/// The link speed for display: zero while disconnected, and zero for the
/// "unknown" markers some drivers report.
pub fn clean_link_speed(bits_per_s: u64, up: bool) -> u64 {
    if !up || bits_per_s == u64::MAX || bits_per_s == u64::from(u32::MAX) {
        0
    } else {
        bits_per_s
    }
}

/// True for the placeholder name servers Windows lists on adapters that have
/// none configured (the retired site-local range).
fn placeholder_dns(address: &str) -> bool {
    address.starts_with("fec0:")
}

// ----------------------------------------------------------------------------------
// Reading this machine
// ----------------------------------------------------------------------------------

struct Entry {
    luid: u64,
    if_type: u32,
    /// Everything that does not change between ticks.
    fixed: NetSample,
    previous: Option<(u64, u64)>,
}

/// Does the actual reading. Lives on the helper thread of [`NetSampler`].
#[derive(Default)]
struct Poller {
    entries: Vec<Entry>,
    refreshed: Option<Instant>,
    last: Option<Instant>,
    address_buf: Vec<u64>,
    row: Box<MIB_IF_ROW2>,
}

/// Copies a socket address out of the adapter list.
fn socket_text(address: &SOCKET_ADDRESS) -> Option<(String, bool)> {
    let len = usize::try_from(address.iSockaddrLength).ok()?;
    if address.lpSockaddr.is_null() || !(8..=128).contains(&len) {
        return None;
    }
    // SAFETY: the pointer and length describe a socket address inside the
    // buffer GetAdaptersAddresses filled, which is alive during this call.
    let bytes = unsafe { std::slice::from_raw_parts(address.lpSockaddr.cast::<u8>(), len) };
    format_sockaddr(bytes)
}

fn wide_text(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: non-null strings in the adapter list are NUL-terminated and
    // live in the buffer that is alive during this call.
    unsafe { text.to_string() }.unwrap_or_default()
}

impl Poller {
    /// Reads one interface row into `self.row`.
    fn read_row(&mut self, luid: u64) -> bool {
        *self.row = MIB_IF_ROW2::default();
        self.row.InterfaceLuid.Value = luid;
        // SAFETY: the row is a valid, writable structure with its LUID set.
        unsafe { GetIfEntry2(&mut *self.row) }.0 == 0
    }

    /// Re-reads the adapter list with names and addresses.
    fn refresh(&mut self) {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
        if self.address_buf.len() < 2048 {
            self.address_buf.resize(2048, 0);
        }
        let mut filled = false;
        for _ in 0..4 {
            let mut size = (self.address_buf.len() * 8).min(u32::MAX as usize) as u32;
            // SAFETY: the buffer is writable for `size` bytes and 8-byte aligned.
            let status = unsafe {
                GetAdaptersAddresses(
                    0,
                    flags,
                    None,
                    Some(
                        self.address_buf
                            .as_mut_ptr()
                            .cast::<IP_ADAPTER_ADDRESSES_LH>(),
                    ),
                    &mut size,
                )
            };
            if status == 0 {
                filled = true;
                break;
            }
            if status != ERROR_BUFFER_OVERFLOW || size as usize > (64 << 20) {
                break;
            }
            self.address_buf.resize(size as usize / 8 + 512, 0);
        }
        self.refreshed = Some(Instant::now());
        if !filled {
            // No list this time (for example no adapters at all).
            self.entries.clear();
            return;
        }

        let mut found: Vec<Entry> = Vec::new();
        let base = self.address_buf.as_ptr() as usize;
        let end = base + self.address_buf.len() * 8;
        let record = std::mem::size_of::<IP_ADAPTER_ADDRESSES_LH>();
        let mut cursor = self.address_buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        for _ in 0..1024 {
            let address = cursor as usize;
            if cursor.is_null()
                || address < base
                || address + record > end
                || !address.is_multiple_of(8)
            {
                break;
            }
            // SAFETY: the record lies inside the buffer the system just filled
            // (checked above) and is suitably aligned.
            let adapter = unsafe { &*cursor };
            cursor = adapter.Next;
            if adapter.IfType == IF_TYPE_LOOPBACK {
                // The built-in software loopback: nothing to show.
                continue;
            }
            // SAFETY: both union members are plain 64-bit values.
            let luid = unsafe { adapter.Luid.Value };
            let mut fixed = NetSample {
                name: wide_text(adapter.FriendlyName),
                description: wide_text(adapter.Description),
                ..Default::default()
            };
            let mac_len =
                (adapter.PhysicalAddressLength as usize).min(adapter.PhysicalAddress.len());
            fixed.mac = format_mac(&adapter.PhysicalAddress[..mac_len]);

            let mut unicast = adapter.FirstUnicastAddress;
            for _ in 0..256 {
                if unicast.is_null() {
                    break;
                }
                // SAFETY: list nodes are valid until the buffer is reused.
                let node = unsafe { &*unicast };
                if let Some((text, v6)) = socket_text(&node.Address) {
                    if v6 {
                        fixed.ipv6.push(text);
                    } else {
                        fixed.ipv4.push(text);
                    }
                }
                unicast = node.Next;
            }
            let mut gateway = adapter.FirstGatewayAddress;
            let mut gateway_v6 = String::new();
            for _ in 0..64 {
                if gateway.is_null() {
                    break;
                }
                // SAFETY: as above.
                let node = unsafe { &*gateway };
                if let Some((text, v6)) = socket_text(&node.Address) {
                    if !v6 && fixed.gateway.is_empty() {
                        fixed.gateway = text;
                    } else if v6 && gateway_v6.is_empty() {
                        gateway_v6 = text;
                    }
                }
                gateway = node.Next;
            }
            if fixed.gateway.is_empty() {
                fixed.gateway = gateway_v6;
            }
            let mut dns = adapter.FirstDnsServerAddress;
            for _ in 0..64 {
                if dns.is_null() {
                    break;
                }
                // SAFETY: as above.
                let node = unsafe { &*dns };
                if let Some((text, _)) = socket_text(&node.Address) {
                    if !placeholder_dns(&text) && !fixed.dns.contains(&text) {
                        fixed.dns.push(text);
                    }
                }
                dns = node.Next;
            }
            if adapter.Mtu > 0 && adapter.Mtu != u32::MAX {
                fixed
                    .extra
                    .push(("MTU".to_string(), adapter.Mtu.to_string()));
            }
            let previous = self
                .entries
                .iter()
                .find(|e| e.luid == luid)
                .and_then(|e| e.previous);
            found.push(Entry {
                luid,
                if_type: adapter.IfType,
                fixed,
                previous,
            });
        }
        self.entries = found;
    }

    /// One entry per adapter worth showing (connected or physical).
    fn poll(&mut self) -> Vec<NetSample> {
        let due = self
            .refreshed
            .is_none_or(|at| at.elapsed() >= ADDRESS_PERIOD);
        if due {
            self.refresh();
        }
        let now = Instant::now();
        let seconds = self
            .last
            .map(|at| now.duration_since(at).as_secs_f64())
            .unwrap_or(0.0);
        self.last = Some(now);

        let mut out = Vec::with_capacity(self.entries.len());
        for index in 0..self.entries.len() {
            let luid = self.entries[index].luid;
            let have_row = self.read_row(luid);
            let entry = &mut self.entries[index];
            let row = &*self.row;
            let (hardware, medium, up) = if have_row {
                (
                    // Bit 0 of the flags: a hardware interface.
                    Some(row.InterfaceAndOperStatusFlags._bitfield & 1 != 0),
                    row.PhysicalMediumType.0 as u32,
                    row.OperStatus.0 == IF_OPER_STATUS_UP,
                )
            } else {
                (None, 0, false)
            };
            let kind = classify(entry.if_type, hardware, medium, &entry.fixed.description);
            if !worth_showing(kind, up) {
                entry.previous = None;
                continue;
            }
            let mut sample = entry.fixed.clone();
            sample.kind = kind.to_string();
            sample.up = up;
            if have_row {
                sample.link_bps = clean_link_speed(row.ReceiveLinkSpeed, up);
                let transmit = clean_link_speed(row.TransmitLinkSpeed, up);
                if transmit != 0 && transmit != sample.link_bps {
                    sample.extra.push((
                        "Transmit link speed".to_string(),
                        crate::util::fmt_bits(transmit),
                    ));
                }
                sample.rx_total = row.InOctets;
                sample.tx_total = row.OutOctets;
                if let Some((rx, tx)) = entry.previous {
                    sample.rx_bps = rate(rx, row.InOctets, seconds);
                    sample.tx_bps = rate(tx, row.OutOctets, seconds);
                }
                entry.previous = Some((row.InOctets, row.OutOctets));
            } else {
                entry.previous = None;
            }
            out.push(sample);
        }
        out.sort_by(|a, b| {
            (sort_rank(&a.kind, a.up), &a.name).cmp(&(sort_rank(&b.kind, b.up), &b.name))
        });
        out
    }
}

/// How long [`NetSampler::sample`] waits for a fresh reading before it
/// returns the previous one.
const FRESH_WAIT: Duration = Duration::from_millis(25);

/// The helper thread: one reading per request, until the sampler is dropped.
struct Worker {
    requests: mpsc::Sender<()>,
    results: mpsc::Receiver<Vec<NetSample>>,
}

fn worker_loop(requests: mpsc::Receiver<()>, results: mpsc::Sender<Vec<NetSample>>) {
    let mut poller = Poller::default();
    while requests.recv().is_ok() {
        // Requests that piled up during a slow reading need only one answer.
        while requests.try_recv().is_ok() {}
        if results.send(poller.poll()).is_err() {
            return;
        }
    }
}

/// Live adapter readings.
///
/// The counters are read on a helper thread, because a network driver can
/// take hundreds of milliseconds to answer and that must not hold up the
/// rest of the sampling. Each call asks for a new reading and waits a moment
/// for it; when the drivers are slow it returns the previous reading instead
/// (the rates are always computed over the real time between two readings).
#[derive(Default)]
pub struct NetSampler {
    worker: Option<Worker>,
    /// Used when the helper thread could not be started.
    inline: Option<Poller>,
    latest: Vec<NetSample>,
}

impl NetSampler {
    pub fn new() -> NetSampler {
        NetSampler::default()
    }

    fn start(&mut self) {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pulseforge-net".into())
            .spawn(move || worker_loop(request_rx, result_tx));
        match spawned {
            Ok(_) => {
                self.worker = Some(Worker {
                    requests: request_tx,
                    results: result_rx,
                });
            }
            Err(_) => self.inline = Some(Poller::default()),
        }
    }

    /// One entry per adapter worth showing (connected or physical).
    pub fn sample(&mut self) -> Vec<NetSample> {
        if self.worker.is_none() && self.inline.is_none() {
            self.start();
        }
        if let Some(poller) = &mut self.inline {
            return poller.poll();
        }
        let Some(worker) = &self.worker else {
            return Vec::new();
        };
        // The very first reading opens the adapter list, which takes longer.
        let wait = if self.latest.is_empty() {
            FRESH_WAIT * 20
        } else {
            FRESH_WAIT
        };
        // Anything that arrived late since the last call.
        while let Ok(list) = worker.results.try_recv() {
            self.latest = list;
        }
        let alive = worker.requests.send(()).is_ok();
        match worker.results.recv_timeout(wait) {
            Ok(list) => self.latest = list,
            Err(mpsc::RecvTimeoutError::Timeout) if alive => {}
            Err(_) => {
                // The helper thread is gone: read on this thread from now on.
                self.worker = None;
                let mut poller = Poller::default();
                self.latest = poller.poll();
                self.inline = Some(poller);
            }
        }
        self.latest.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        // Plain hardware.
        assert_eq!(
            classify(6, Some(true), 14, "Example Gigabit Controller"),
            "Ethernet"
        );
        assert_eq!(
            classify(71, Some(true), 9, "Example Wi-Fi 6 Adapter"),
            "Wi-Fi"
        );
        assert_eq!(
            classify(6, Some(true), 9, "Example Wireless Adapter"),
            "Wi-Fi"
        );
        assert_eq!(
            classify(243, Some(true), 8, "Example Mobile Broadband"),
            "Cellular"
        );
        assert_eq!(
            classify(244, Some(true), 0, "Example LTE Modem"),
            "Cellular"
        );
        assert_eq!(
            classify(6, Some(true), 10, "Personal Area Network"),
            "Bluetooth"
        );
        assert_eq!(
            classify(6, Some(true), 0, "Bluetooth Device (Personal Area Network)"),
            "Bluetooth"
        );
        assert_eq!(
            classify(144, Some(true), 0, "Example Serial Bus Adapter"),
            "Other"
        );

        // No hardware behind it: virtual switch ports, VPN adapters.
        assert_eq!(
            classify(6, Some(false), 0, "Example Virtual Ethernet Adapter"),
            "Virtual"
        );
        assert_eq!(
            classify(6, Some(false), 14, "Example Secure Client Adapter"),
            "Virtual"
        );
        assert_eq!(
            classify(71, Some(false), 9, "Example Wi-Fi Direct Virtual Adapter"),
            "Virtual"
        );
        assert_eq!(
            classify(131, Some(false), 0, "Example Tunneling Adapter"),
            "Virtual"
        );
        assert_eq!(classify(23, Some(true), 0, "Dial-up Link"), "Virtual");
        assert_eq!(classify(53, None, 0, "Something"), "Virtual");

        // A machine that is itself virtual has a real adapter whose name says
        // "virtual": Windows reports hardware, so its traffic must count.
        assert_eq!(
            classify(
                6,
                Some(true),
                0,
                "Example Virtual Machine Bus Network Adapter"
            ),
            "Ethernet"
        );

        // Without the answer of Windows the description decides.
        assert_eq!(
            classify(6, None, 0, "Example Virtual Ethernet Adapter"),
            "Virtual"
        );
        assert_eq!(classify(6, None, 0, "Example VPN Client"), "Virtual");
        assert_eq!(classify(6, None, 0, "TAP-Example Adapter V9"), "Virtual");
        assert_eq!(classify(6, None, 0, "WAN Miniport (IP)"), "Virtual");
        assert_eq!(
            classify(6, None, 0, "Example Gigabit Controller"),
            "Ethernet"
        );

        // Loopback, software or test adapter.
        assert_eq!(
            classify(24, Some(false), 0, "Software Loopback Interface 1"),
            "Loopback"
        );
        assert_eq!(
            classify(6, Some(false), 0, "Example KM-TEST Loopback Adapter"),
            "Loopback"
        );

        // Nonsense input still gives one of the known words.
        assert_eq!(classify(u32::MAX, None, u32::MAX, ""), "Other");
        assert_eq!(classify(0, Some(true), 0, "\u{FFFD}"), "Other");
    }

    #[test]
    fn what_is_shown_and_counted() {
        assert!(
            worth_showing("Ethernet", false),
            "physical adapters show as disconnected"
        );
        assert!(worth_showing("Wi-Fi", true));
        assert!(worth_showing("Virtual", true));
        assert!(!worth_showing("Virtual", false));
        assert!(!worth_showing("Loopback", false));
        assert!(is_physical("Bluetooth") && is_physical("Cellular") && is_physical("Other"));
        assert!(!is_physical("Virtual") && !is_physical("Loopback"));

        // The totals of the model skip exactly the kinds that are not physical.
        let sample = crate::model::Sample {
            net: ["Ethernet", "Wi-Fi", "Virtual", "Loopback", "Bluetooth"]
                .iter()
                .map(|kind| NetSample {
                    kind: (*kind).to_string(),
                    rx_bps: 100.0,
                    tx_bps: 10.0,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(sample.net_totals(), (300.0, 30.0));

        assert!(sort_rank("Ethernet", true) < sort_rank("Wi-Fi", true));
        assert!(sort_rank("Wi-Fi", true) < sort_rank("Ethernet", false));
        assert!(sort_rank("Ethernet", false) < sort_rank("Virtual", true));
    }

    #[test]
    fn addresses_format() {
        assert_eq!(
            format_mac(&[0x02, 0x00, 0x5E, 0x10, 0xAB, 0xFF]),
            "02-00-5E-10-AB-FF"
        );
        assert_eq!(format_mac(&[]), "");
        assert_eq!(format_mac(&[0; 6]), "");
        assert_eq!(format_mac(&[0xFF; 33]), "");
        assert_eq!(format_mac(&[7]), "07");

        // IPv4 socket address: family 2, port, then 192.0.2.44.
        let mut v4 = [0u8; 16];
        v4[0] = 2;
        v4[4..8].copy_from_slice(&[192, 0, 2, 44]);
        assert_eq!(
            format_sockaddr(&v4),
            Some(("192.0.2.44".to_string(), false))
        );
        assert_eq!(format_sockaddr(&v4[..7]), None, "truncated");

        // IPv6 socket address: family 23, port, flow, then 2001:db8::1.
        let mut v6 = [0u8; 28];
        v6[0] = 23;
        v6[8..12].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
        v6[23] = 1;
        assert_eq!(
            format_sockaddr(&v6),
            Some(("2001:db8::1".to_string(), true))
        );
        assert_eq!(format_sockaddr(&v6[..23]), None, "truncated");

        assert_eq!(format_sockaddr(&[]), None);
        assert_eq!(format_sockaddr(&[2]), None);
        assert_eq!(format_sockaddr(&[0u8; 28]), None, "family 0");
        assert_eq!(format_sockaddr(&[0xFFu8; 28]), None, "unknown family");

        assert!(placeholder_dns("fec0:0:0:ffff::1"));
        assert!(!placeholder_dns("2001:db8::53"));
        assert!(!placeholder_dns("192.0.2.53"));
    }

    #[test]
    fn link_speed_and_rates() {
        assert_eq!(clean_link_speed(1_000_000_000, true), 1_000_000_000);
        assert_eq!(clean_link_speed(1_000_000_000, false), 0);
        assert_eq!(clean_link_speed(u64::MAX, true), 0);
        assert_eq!(clean_link_speed(u64::from(u32::MAX), true), 0);
        assert_eq!(clean_link_speed(0, true), 0);

        // The byte counters are 64-bit totals: a smaller value means the
        // adapter was reset, which must not show as a huge rate.
        assert_eq!(rate(1_000, 501_000, 0.5), 1_000_000.0);
        assert_eq!(rate(501_000, 1_000, 1.0), 0.0);
        assert_eq!(rate(1_000, 501_000, 0.0), 0.0);
    }

    #[test]
    fn live_smoke() {
        let mut sampler = NetSampler::new();
        let first = sampler.sample();
        assert!(first.iter().all(|n| n.rx_bps == 0.0 && n.tx_bps == 0.0));
        std::thread::sleep(Duration::from_millis(400));
        let started = Instant::now();
        let mut list = Vec::new();
        for _ in 0..20 {
            list = sampler.sample();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let mut poller = Poller::default();
        let started = Instant::now();
        poller.refresh();
        let refresh_ms = started.elapsed().as_secs_f64() * 1000.0;
        let direct = poller.poll();
        assert_eq!(
            direct.len(),
            list.len(),
            "both paths list the same adapters"
        );

        const KINDS: [&str; 7] = [
            "Ethernet",
            "Wi-Fi",
            "Cellular",
            "Bluetooth",
            "Virtual",
            "Loopback",
            "Other",
        ];
        for adapter in &list {
            assert!(KINDS.contains(&adapter.kind.as_str()));
            assert!(adapter.rx_bps >= 0.0 && adapter.tx_bps >= 0.0);
            assert!(adapter.rx_bps.is_finite() && adapter.tx_bps.is_finite());
            assert!(adapter.up || is_physical(&adapter.kind));
            assert!(!adapter.name.is_empty());
            if !adapter.up {
                assert_eq!(adapter.link_bps, 0);
            }
        }
        let mut names: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), list.len(), "no adapter is listed twice");
        let physical_up = list.iter().filter(|n| n.up && is_physical(&n.kind)).count();
        println!(
            "network: {} shown, {} physical and connected, {} virtual, {} with an address, {} with a gateway, avg {ms:.3} ms per call, address refresh {refresh_ms:.2} ms",
            list.len(),
            physical_up,
            list.iter().filter(|n| n.kind == "Virtual").count(),
            list.iter().filter(|n| !n.ipv4.is_empty() || !n.ipv6.is_empty()).count(),
            list.iter().filter(|n| !n.gateway.is_empty()).count(),
        );
    }
}
