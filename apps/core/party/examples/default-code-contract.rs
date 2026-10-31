//! Mirrors Bilimbi Core.Party's shared default-code suggestion behavior.
use bilibi_party::{default_code, CodeKind};

fn main() {
    let cases = [
        ("Person (M)", CodeKind::Person, "person_m"),
        ("Al Pacino", CodeKind::Person, "al_pacino"),
        ("So Ji Sub", CodeKind::Person, "so_ji_sub"),
        ("Muhammad Hafiz bin Ismail", CodeKind::Person, "muhammad"),
        ("Kamala a/p Muthu", CodeKind::Person, "kamala_muthu"),
        (
            "PONG CODAN RUBBER (M) SDN. BHD.",
            CodeKind::Company,
            "pong_codan",
        ),
        ("Scicom (MSC) Berhad", CodeKind::Company, "scicom_msc"),
        (
            "The Co-operative Group Limited",
            CodeKind::Company,
            "the_co",
        ),
    ];

    for (name, kind, expected) in cases {
        assert_eq!(default_code(name, kind, 1), expected, "{name}");
    }
}
