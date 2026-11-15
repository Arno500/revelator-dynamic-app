//! Descriptor model and vendor-interface selection (pure logic, unit-tested).

use nusb::descriptors::Configuration;
use nusb::transfer::EndpointType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpKind {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDesc {
    pub address: u8,
    pub kind: EpKind,
    pub max_packet: u16,
    pub interval: u8,
}

impl EndpointDesc {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltDesc {
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<EndpointDesc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDesc {
    pub number: u8,
    pub alts: Vec<AltDesc>,
}

/// A usable vendor-specific bulk interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorEndpoints {
    pub interface: u8,
    pub alt: u8,
    pub out_ep: u8,
    pub in_ep: u8,
    pub out_max_packet: u16,
    pub in_max_packet: u16,
}

/// Interface number the io24 reference uses; only a tie-break preference.
const PREFERRED_INTERFACE: u8 = 5;

/// All (interface, alt) pairs that are class 0xFF and expose a bulk IN + bulk OUT pair.
pub fn vendor_candidates(ifaces: &[InterfaceDesc]) -> Vec<VendorEndpoints> {
    let mut out = Vec::new();
    for i in ifaces {
        for a in &i.alts {
            if a.class != 0xFF {
                continue;
            }
            let bulk = |inn: bool| {
                a.endpoints
                    .iter()
                    .find(|e| e.kind == EpKind::Bulk && e.is_in() == inn)
            };
            if let (Some(o), Some(n)) = (bulk(false), bulk(true)) {
                out.push(VendorEndpoints {
                    interface: i.number,
                    alt: a.alt,
                    out_ep: o.address,
                    in_ep: n.address,
                    out_max_packet: o.max_packet,
                    in_max_packet: n.max_packet,
                });
            }
        }
    }
    out
}

/// Pick the vendor interface: prefer interface 5 (io24 hypothesis) if it qualifies,
/// otherwise the lowest-numbered one; within an interface the lowest alt setting.
pub fn select_vendor_interface(ifaces: &[InterfaceDesc]) -> Option<VendorEndpoints> {
    vendor_candidates(ifaces)
        .into_iter()
        .min_by_key(|c| (c.interface != PREFERRED_INTERFACE, c.interface, c.alt))
}

/// Convert nusb's parsed configuration into our plain model.
pub fn parse_config(cfg: &Configuration<'_>) -> Vec<InterfaceDesc> {
    cfg.interfaces()
        .map(|g| InterfaceDesc {
            number: g.interface_number(),
            alts: g
                .alt_settings()
                .map(|a| AltDesc {
                    alt: a.alternate_setting(),
                    class: a.class(),
                    subclass: a.subclass(),
                    protocol: a.protocol(),
                    endpoints: a
                        .endpoints()
                        .map(|e| EndpointDesc {
                            address: e.address(),
                            kind: match e.transfer_type() {
                                EndpointType::Control => EpKind::Control,
                                EndpointType::Isochronous => EpKind::Isochronous,
                                EndpointType::Bulk => EpKind::Bulk,
                                EndpointType::Interrupt => EpKind::Interrupt,
                            },
                            max_packet: e.max_packet_size() as u16,
                            interval: e.interval(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(address: u8, kind: EpKind) -> EndpointDesc {
        EndpointDesc {
            address,
            kind,
            max_packet: 512,
            interval: 0,
        }
    }
    fn alt(alt: u8, class: u8, eps: Vec<EndpointDesc>) -> AltDesc {
        AltDesc {
            alt,
            class,
            subclass: 0,
            protocol: 0,
            endpoints: eps,
        }
    }

    fn io24_like() -> Vec<InterfaceDesc> {
        vec![
            InterfaceDesc {
                number: 0,
                alts: vec![
                    alt(0, 1, vec![]),
                    alt(1, 1, vec![ep(0x02, EpKind::Isochronous)]),
                ],
            },
            InterfaceDesc {
                number: 3,
                alts: vec![alt(
                    0,
                    1,
                    vec![ep(0x03, EpKind::Bulk), ep(0x83, EpKind::Bulk)],
                )],
            },
            InterfaceDesc {
                number: 5,
                alts: vec![
                    alt(0, 0xFF, vec![]),
                    alt(
                        1,
                        0xFF,
                        vec![ep(0x01, EpKind::Bulk), ep(0x81, EpKind::Bulk)],
                    ),
                ],
            },
        ]
    }

    #[test]
    fn selects_io24_layout() {
        let s = select_vendor_interface(&io24_like()).unwrap();
        assert_eq!((s.interface, s.alt, s.out_ep, s.in_ep), (5, 1, 0x01, 0x81));
    }

    #[test]
    fn ignores_non_vendor_bulk_and_needs_both_directions() {
        let v = vec![
            InterfaceDesc {
                number: 2,
                alts: vec![alt(
                    0,
                    8,
                    vec![ep(0x01, EpKind::Bulk), ep(0x81, EpKind::Bulk)],
                )],
            },
            InterfaceDesc {
                number: 4,
                alts: vec![alt(0, 0xFF, vec![ep(0x01, EpKind::Bulk)])],
            },
            InterfaceDesc {
                number: 6,
                alts: vec![alt(
                    0,
                    0xFF,
                    vec![ep(0x81, EpKind::Interrupt), ep(0x02, EpKind::Bulk)],
                )],
            },
        ];
        assert!(select_vendor_interface(&v).is_none());
    }

    #[test]
    fn works_when_interface_is_not_five_and_prefers_five_when_present() {
        let mut v = vec![InterfaceDesc {
            number: 2,
            alts: vec![
                alt(0, 0xFF, vec![]),
                alt(
                    2,
                    0xFF,
                    vec![ep(0x04, EpKind::Bulk), ep(0x84, EpKind::Bulk)],
                ),
            ],
        }];
        let s = select_vendor_interface(&v).unwrap();
        assert_eq!((s.interface, s.alt, s.out_ep, s.in_ep), (2, 2, 0x04, 0x84));
        v.push(InterfaceDesc {
            number: 5,
            alts: vec![alt(
                1,
                0xFF,
                vec![ep(0x01, EpKind::Bulk), ep(0x81, EpKind::Bulk)],
            )],
        });
        assert_eq!(select_vendor_interface(&v).unwrap().interface, 5);
        assert_eq!(vendor_candidates(&v).len(), 2);
    }

    #[test]
    fn parses_raw_config_descriptor() {
        // config (9) + iface 5 alt 1 vendor (9) + 2 bulk endpoints (7 each)
        let mut d: Vec<u8> = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 50];
        d.extend([9, 4, 5, 1, 2, 0xFF, 0, 0, 0]);
        d.extend([7, 5, 0x01, 2, 0x00, 0x02, 0]);
        d.extend([7, 5, 0x81, 2, 0x00, 0x02, 0]);
        let total = d.len() as u16;
        d[2..4].copy_from_slice(&total.to_le_bytes());
        let cfg = Configuration::new(&d);
        let parsed = parse_config(&cfg);
        let s = select_vendor_interface(&parsed).unwrap();
        assert_eq!(
            (s.interface, s.alt, s.out_ep, s.in_ep, s.in_max_packet),
            (5, 1, 0x01, 0x81, 512)
        );
    }
}
