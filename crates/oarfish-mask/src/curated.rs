//! The bundle that ships with oarfish.
//!
//! Embedded at compile time, so a default install needs no files on disk and
//! cannot start with a bundle that does not parse: a malformed default fails
//! this crate's tests long before it reaches anyone.

use std::sync::OnceLock;

use crate::Bundle;

const DEFAULT_BUNDLE: &str = include_str!("../bundle/default.toml");

/// The curated default bundle, parsed and compiled once per process.
///
/// Panics if the embedded bundle is invalid, which is unreachable in a build
/// whose tests passed and is not a condition a caller could handle anyway.
pub fn curated() -> &'static Bundle {
    static CURATED: OnceLock<Bundle> = OnceLock::new();
    CURATED.get_or_init(|| {
        Bundle::parse(DEFAULT_BUNDLE).expect("the embedded default bundle must be valid")
    })
}

#[cfg(test)]
mod tests {
    use crate::curated;

    #[test]
    fn the_curated_bundle_loads() {
        let bundle = curated();
        assert_eq!(bundle.version(), 1);
        assert_eq!(bundle.slots().len(), 15);
    }

    #[test]
    fn it_is_parsed_once_and_shared() {
        assert!(std::ptr::eq(curated(), curated()));
    }

    #[test]
    fn every_slot_records_why_it_exists() {
        for slot in curated().slots() {
            assert!(
                !slot.why.trim().is_empty(),
                "slot {} has no recorded reason",
                slot.name
            );
        }
    }

    #[rstest::rstest]
    #[case("2026-09-18T03:14:07.221Z ready", "<VAR:TS> ready")]
    #[case("id 3f2a9c1e-88b4-4f1a-9c2d-0a1b2c3d4e5f", "id <VAR:UUID>")]
    #[case("link 00:1b:44:11:3a:b7 down", "link <VAR:MAC> down")]
    #[case("peer fe80::1c2d:3e4f:5a6b:7c8d", "peer <VAR:IP6>")]
    #[case("from 192.168.1.44 port 22", "from <VAR:IP4> port <VAR:NUM>")]
    #[case("GET https://example.com/a/b failed", "GET <VAR:URL> failed")]
    #[case("mail to ops@example.com bounced", "mail to <VAR:EMAIL> bounced")]
    #[case("device /dev/sda1 offline", "device <VAR:DEV> offline")]
    #[case("reading /var/log/syslog failed", "reading <VAR:PATH> failed")]
    #[case("fault at 0xffff8881", "fault at <VAR:HEX>")]
    #[case("wrote 4.2 GB", "wrote <VAR:SIZE>")]
    #[case("took 340ms", "took <VAR:DUR>")]
    #[case("sshd[12345]: start", "sshd<VAR:PID>: start")]
    #[case("retry 7 of 9", "retry <VAR:NUM> of <VAR:NUM>")]
    fn each_slot_masks_what_it_claims(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(curated().mask(line).template(), expected);
    }

    /// Named months and weekdays belong to the timestamp. Left literal, a log
    /// crossing a month end splits every template it has.
    #[rstest::rstest]
    #[case(
        "Jun 14 15:16:01 combo sshd[19939]: ok",
        "<VAR:TS> combo sshd<VAR:PID>: ok"
    )]
    #[case("Sep  2 03:00:00 pve kernel: ok", "<VAR:TS> pve kernel: ok")]
    #[case("[Sun Dec 04 04:47:44 2005] [notice] ok", "[<VAR:TS>] [notice] ok")]
    #[case("the June report", "the June report")]
    fn named_dates_are_timestamps(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(curated().mask(line).template(), expected);
    }

    /// IP6 takes real address shapes only: colon-joined clocks and
    /// `name:id:name` tokens are not addresses.
    #[rstest::rstest]
    #[case("peer ::1 closed", "peer <VAR:IP6> closed")]
    #[case("peer ::ffff:10.0.0.1 closed", "peer <VAR:IP6> closed")]
    #[case("peer 2001:db8:0:0:0:0:2:1 closed", "peer <VAR:IP6> closed")]
    #[case("at 22:15:29:606 ok", "at <VAR:TS>:<VAR:NUM> ok")]
    #[case("[SendWorker:188978561024:Quorum]", "[SendWorker:<VAR:NUM>:Quorum]")]
    fn ip6_takes_real_addresses_only(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(curated().mask(line).template(), expected);
    }

    /// An all-digit run is a number at any length; HEX needs a letter.
    #[rstest::rstest]
    #[case("seq 1234567 ok", "seq <VAR:NUM> ok")]
    #[case("seq 12345678 ok", "seq <VAR:NUM> ok")]
    #[case("id 1234567a ok", "id <VAR:HEX> ok")]
    #[case("id deadbeef ok", "id <VAR:HEX> ok")]
    #[case("the word acceded", "the word acceded")]
    #[case("wal 000000010000000000000042 ok", "wal <VAR:HEX> ok")]
    #[case("max 18446744073709551615 ok", "max <VAR:NUM> ok")]
    fn hex_needs_a_letter(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(curated().mask(line).template(), expected);
    }

    /// DNS names are variables; dotted code names are the logger's identity.
    #[rstest::rstest]
    #[case(
        "reverse mapping for ns.example.com failed",
        "reverse mapping for <VAR:HOST> failed"
    )]
    #[case("rhost=massive.merukuru.org", "rhost=<VAR:HOST>")]
    #[case("proxy.cse.cuhk.edu.hk:5070 open", "<VAR:HOST>:<VAR:NUM> open")]
    #[case("lookup nas.lan ok", "lookup <VAR:HOST> ok")]
    #[case(
        "org.apache.hadoop.mapred.MapTask: done",
        "org.apache.hadoop.mapred.MapTask: done"
    )]
    #[case("nova.compute.manager started", "nova.compute.manager started")]
    #[case("see README.md", "see README.md")]
    #[case(
        "mapreduce.v2.app.rm.RMContainerAllocator: ok",
        "mapreduce.v2.app.rm.RMContainerAllocator: ok"
    )]
    #[case("mail to ops@example.com bounced", "mail to <VAR:EMAIL> bounced")]
    fn host_takes_dns_names_only(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(curated().mask(line).template(), expected);
    }

    /// The first ordering that carries risk. PATH would happily eat /dev/sda1.
    #[test]
    fn dev_beats_path() {
        assert_eq!(
            curated().mask("smartd: Device: /dev/sda [SAT]").template(),
            "smartd: Device: <VAR:DEV> [SAT]"
        );
    }

    /// The second. Everything numeric is declared before NUM.
    #[test]
    fn specific_numeric_slots_beat_num() {
        let bundle = curated();
        assert_eq!(bundle.mask("freed 512 MiB").template(), "freed <VAR:SIZE>");
        assert_eq!(bundle.mask("waited 1.5s").template(), "waited <VAR:DUR>");
        assert_eq!(
            bundle.mask("at 10.0.0.1 now").template(),
            "at <VAR:IP4> now"
        );
    }

    /// Digits inside a word identify the word. Destroy these and the operator
    /// loses the one token that says which filesystem and which controller.
    #[test]
    fn digits_inside_a_word_are_left_alone() {
        let bundle = curated();
        let masked = bundle.mask("EXT4-fs error (device sda1): inode #98304");
        assert_eq!(
            masked.template(),
            "EXT4-fs error (device <VAR:DEV>): inode #<VAR:NUM>"
        );
        assert!(masked.template().contains("EXT4-fs"));

        assert_eq!(
            bundle.mask("nvme nvme0: I/O 442 timeout").template(),
            "nvme nvme0: I/O <VAR:NUM> timeout"
        );
    }

    /// The word `vdev` is not a device. It was, until the DEV pattern was tightened.
    #[test]
    fn vdev_is_a_word_not_a_device() {
        assert_eq!(
            curated()
                .mask("pool tank vdev nvme0n1p2 degraded")
                .template(),
            "pool tank vdev <VAR:DEV> degraded"
        );
    }

    #[test]
    fn the_curated_bundle_masks_idempotently() {
        let bundle = curated();
        for line in [
            "EXT4-fs error (device sda1): inode #98304",
            "Failed password for admin from 192.168.1.44 port 54321 ssh2",
            "2026-09-18T03:14:07.221Z kernel: nvme nvme0: I/O 442 timeout",
            "smartd: /dev/sda temperature changed from 38 to 41",
        ] {
            let once = bundle.mask(line).template().to_owned();
            let twice = bundle.mask(&once).template().to_owned();
            assert_eq!(once, twice, "not idempotent: {line}");
        }
    }
}
