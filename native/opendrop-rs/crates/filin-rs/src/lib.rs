pub mod radiotap {
    #[derive(Debug, PartialEq, Eq)]
    pub struct RadiotapHeader<'a> {
        pub header_len: usize,
        pub payload: &'a [u8],
        pub tsft: Option<u64>,
        pub flags: Option<u8>,
        pub antenna_signal_dbm: Option<i8>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum RadiotapError {
        TooShort,
        UnsupportedVersion(u8),
        InvalidLength { header_len: usize, frame_len: usize },
        UnsupportedField(u8),
    }

    pub fn parse_header(frame: &[u8]) -> Result<RadiotapHeader<'_>, RadiotapError> {
        if frame.len() < 8 {
            return Err(RadiotapError::TooShort);
        }
        if frame[0] != 0 {
            return Err(RadiotapError::UnsupportedVersion(frame[0]));
        }

        let header_len = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        if header_len < 8 || header_len > frame.len() {
            return Err(RadiotapError::InvalidLength {
                header_len,
                frame_len: frame.len(),
            });
        }

        let mut offset = 4;
        let mut present_words = Vec::new();
        loop {
            let word = read_field::<4>(frame, header_len, offset)?;
            let present = u32::from_le_bytes(word);
            present_words.push(present);
            offset += 4;
            if present & (1 << 31) == 0 {
                break;
            }
        }

        let mut tsft = None;
        let mut flags = None;
        let mut antenna_signal_dbm = None;

        'fields: for (word_index, present) in present_words.into_iter().enumerate() {
            for bit in 0..31 {
                if present & (1 << bit) == 0 {
                    continue;
                }

                let Some(field) = field_layout(word_index, bit) else {
                    break 'fields;
                };
                offset = align(offset, field.align);
                let bytes = read_slice(frame, header_len, offset, field.size)?;

                match (word_index, bit) {
                    (0, 0) => {
                        tsft = Some(u64::from_le_bytes(
                            bytes.try_into().expect("TSFT length is 8 bytes"),
                        ));
                    }
                    (0, 1) => flags = Some(bytes[0]),
                    (0, 5) => antenna_signal_dbm = Some(bytes[0] as i8),
                    _ => {}
                }

                offset += field.size;
            }
        }

        Ok(RadiotapHeader {
            header_len,
            payload: &frame[header_len..],
            tsft,
            flags,
            antenna_signal_dbm,
        })
    }

    /// Build the radiotap TX header. Adds the **TX-flags** (bit 15) and
    /// **data-retries** (bit 17) fields on top of the rate (bit 2).
    ///
    /// For `want_ack` (UNICAST) frames we leave `NOACK` clear and request
    /// retries, so the driver/hardware retransmits on a missing 802.11 ACK.
    /// The peer DOES MAC-ACK our injected unicast frames (confirmed by RF
    /// capture: 50k+ ACKs to our addr), but monitor injection was previously
    /// fire-and-forget — 0 retries — so a lost frame near the end of a bulk
    /// transfer was only recovered by slow TCP (the ~120s endgame stall). For
    /// multicast/broadcast (`want_ack=false`) we set `NOACK` (never ACKed).
    pub fn build_tx_header(want_ack: bool) -> Vec<u8> {
        // IEEE80211_RADIOTAP_F_TX_NOACK = 0x0008.
        let (tx_flags, retries): (u16, u8) = if want_ack { (0x0000, 7) } else { (0x0008, 0) };
        let mut v = vec![
            0x00, 0x00, // version, pad
            0x0d, 0x00, // length = 13
            0x04, 0x80, 0x02, 0x00, // present: rate(2) | TX flags(15) | data retries(17)
            0x18, // rate: 12 Mbps (500 kbps units)
            0x00, // pad to 2-byte-align the TX-flags field (offset 9 -> 10)
        ];
        v.extend_from_slice(&tx_flags.to_le_bytes());
        v.push(retries);
        v
    }

    fn align(offset: usize, alignment: usize) -> usize {
        debug_assert!(alignment.is_power_of_two());
        (offset + alignment - 1) & !(alignment - 1)
    }

    #[derive(Clone, Copy)]
    struct FieldLayout {
        align: usize,
        size: usize,
    }

    fn field_layout(word_index: usize, bit: usize) -> Option<FieldLayout> {
        if word_index != 0 {
            return None;
        }

        let layout = match bit {
            0 => FieldLayout { align: 8, size: 8 },   // TSFT
            1 => FieldLayout { align: 1, size: 1 },   // flags
            2 => FieldLayout { align: 1, size: 1 },   // rate
            3 => FieldLayout { align: 2, size: 4 },   // channel
            4 => FieldLayout { align: 2, size: 2 },   // FHSS
            5 => FieldLayout { align: 1, size: 1 },   // dBm antenna signal
            6 => FieldLayout { align: 1, size: 1 },   // dBm antenna noise
            7 => FieldLayout { align: 2, size: 2 },   // lock quality
            8 => FieldLayout { align: 2, size: 2 },   // TX attenuation
            9 => FieldLayout { align: 2, size: 2 },   // dB TX attenuation
            10 => FieldLayout { align: 1, size: 1 },  // dBm TX power
            11 => FieldLayout { align: 1, size: 1 },  // antenna
            12 => FieldLayout { align: 1, size: 1 },  // dB antenna signal
            13 => FieldLayout { align: 1, size: 1 },  // dB antenna noise
            14 => FieldLayout { align: 2, size: 2 },  // RX flags
            15 => FieldLayout { align: 2, size: 2 },  // TX flags
            16 => FieldLayout { align: 1, size: 1 },  // RTS retries
            17 => FieldLayout { align: 1, size: 1 },  // data retries
            19 => FieldLayout { align: 1, size: 3 },  // MCS
            20 => FieldLayout { align: 4, size: 8 },  // A-MPDU status
            21 => FieldLayout { align: 2, size: 12 }, // VHT
            22 => FieldLayout { align: 8, size: 12 }, // timestamp
            29 | 30 => return None,                   // namespace switches are not data fields
            _ => return None,
        };
        Some(layout)
    }

    fn read_field<const N: usize>(
        frame: &[u8],
        header_len: usize,
        offset: usize,
    ) -> Result<[u8; N], RadiotapError> {
        let bytes = read_slice(frame, header_len, offset, N)?;
        Ok(bytes.try_into().expect("slice length matches field length"))
    }

    fn read_slice(
        frame: &[u8],
        header_len: usize,
        offset: usize,
        size: usize,
    ) -> Result<&[u8], RadiotapError> {
        let end = offset.checked_add(size).ok_or(RadiotapError::TooShort)?;
        frame
            .get(offset..end)
            .filter(|_| end <= header_len)
            .ok_or(RadiotapError::TooShort)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_tsft_flags_and_rssi_from_v0_header() {
            let frame = [
                0x00, 0x00, 0x12, 0x00, // version, pad, length
                0x23, 0x00, 0x00, 0x00, // present: TSFT, flags, dBm antenna signal
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // TSFT
                0x10, // flags: frame includes FCS
                0xd6, // antenna signal: -42 dBm
                0xde, 0xad, // payload begins after radiotap header
            ];

            let parsed = parse_header(&frame).expect("valid radiotap header");

            assert_eq!(parsed.header_len, 18);
            assert_eq!(parsed.payload, &[0xde, 0xad]);
            assert_eq!(parsed.tsft, Some(0x0102_0304_0506_0708));
            assert_eq!(parsed.flags, Some(0x10));
            assert_eq!(parsed.antenna_signal_dbm, Some(-42));
        }

        #[test]
        fn parses_carl9170_header_with_extra_fields_and_no_tsft() {
            let frame = [
                0x00, 0x00, 0x12, 0x00, // version, pad, length
                0x2e, 0x48, 0x00,
                0x00, // present: flags, rate, channel, signal, antenna, rx flags
                0x10, // flags: frame includes FCS
                0x0c, // rate
                0x64, 0x14, 0x40, 0x01, // channel frequency 5220 + flags
                0xc3, // antenna signal: -61 dBm
                0x05, // antenna
                0x00, 0x00, // RX flags
                0xde, 0xad, // payload begins after radiotap header
            ];

            let parsed = parse_header(&frame).expect("valid carl9170 radiotap header");

            assert_eq!(parsed.header_len, 18);
            assert_eq!(parsed.payload, &[0xde, 0xad]);
            assert_eq!(parsed.tsft, None);
            assert_eq!(parsed.flags, Some(0x10));
            assert_eq!(parsed.antenna_signal_dbm, Some(-61));
        }

        #[test]
        fn builds_tx_radiotap_header_with_ack_and_retries() {
            // Unicast: rate + TX flags (0 = expect ACK) + 7 data retries.
            assert_eq!(
                build_tx_header(true),
                vec![
                    0x00, 0x00, 0x0d, 0x00, // version, pad, length=13
                    0x04, 0x80, 0x02, 0x00, // present: rate | TX flags | data retries
                    0x18, // 12 Mbps
                    0x00, // align pad
                    0x00, 0x00, // TX flags = 0 (NOACK clear → expect ACK + retry)
                    0x07, // 7 data retries
                ]
            );
            // Multicast/broadcast: NOACK set (0x0008), 0 retries.
            assert_eq!(&build_tx_header(false)[10..13], &[0x08, 0x00, 0x00]);
        }
    }
}

pub mod ieee80211 {
    #[derive(Debug, PartialEq, Eq)]
    pub struct MacHeader<'a> {
        pub frame_control: u16,
        pub duration_id: u16,
        pub destination: [u8; 6],
        pub source: [u8; 6],
        pub bssid: [u8; 6],
        pub sequence_control: u16,
        /// QoS control field for QoS data frames (owl reads it at offset 24,
        /// owl/src/rx.c:537). `None` for non-QoS frames.
        pub qos_control: Option<u16>,
        pub body: &'a [u8],
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ParseError {
        TooShort,
    }

    pub fn parse_mac_header(frame: &[u8]) -> Result<MacHeader<'_>, ParseError> {
        if frame.len() < 24 {
            return Err(ParseError::TooShort);
        }
        let frame_control = u16::from_le_bytes([frame[0], frame[1]]);
        // QoS data subtypes carry a 2-byte QoS control field after the
        // sequence control; owl strips it before the frame body (owl/src/rx.c:536-541).
        let body_offset = if is_qos_data(frame_control) { 26 } else { 24 };
        if frame.len() < body_offset {
            return Err(ParseError::TooShort);
        }

        Ok(MacHeader {
            frame_control,
            duration_id: u16::from_le_bytes([frame[2], frame[3]]),
            destination: read_addr(frame, 4)?,
            source: read_addr(frame, 10)?,
            bssid: read_addr(frame, 16)?,
            sequence_control: u16::from_le_bytes([frame[22], frame[23]]),
            qos_control: if is_qos_data(frame_control) {
                Some(u16::from_le_bytes([frame[24], frame[25]]))
            } else {
                None
            },
            body: &frame[body_offset..],
        })
    }

    /// True for data frames whose subtype carries a QoS control field
    /// (802.11 STYPE_QOS_DATA bit, owl/src/rx.c:536).
    pub fn is_qos_data(frame_control: u16) -> bool {
        const FTYPE_DATA: u16 = 0x0008;
        const STYPE_QOS: u16 = 0x0080;
        (frame_control & (FTYPE_DATA | STYPE_QOS)) == (FTYPE_DATA | STYPE_QOS)
    }

    /// owl `IEEE80211_QOS_CTL_A_MSDU_PRESENT` (owl/src/ieee80211.h:125).
    pub const QOS_A_MSDU_PRESENT: u16 = 0x0080;

    pub fn build_management_action_frame(
        destination: [u8; 6],
        source: [u8; 6],
        bssid: [u8; 6],
        sequence_control: u16,
        body: &[u8],
    ) -> Vec<u8> {
        let mut frame = Vec::with_capacity(24 + body.len());
        frame.extend_from_slice(&0x00d0u16.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(&destination);
        frame.extend_from_slice(&source);
        frame.extend_from_slice(&bssid);
        frame.extend_from_slice(&sequence_control.to_le_bytes());
        frame.extend_from_slice(body);
        frame
    }

    pub fn build_data_frame(
        destination: [u8; 6],
        source: [u8; 6],
        bssid: [u8; 6],
        sequence_control: u16,
        body: &[u8],
    ) -> Vec<u8> {
        let mut frame = Vec::with_capacity(24 + body.len());
        frame.extend_from_slice(&0x0008u16.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(&destination);
        frame.extend_from_slice(&source);
        frame.extend_from_slice(&bssid);
        frame.extend_from_slice(&sequence_control.to_le_bytes());
        frame.extend_from_slice(body);
        frame
    }

    fn read_addr(frame: &[u8], offset: usize) -> Result<[u8; 6], ParseError> {
        Ok(frame
            .get(offset..offset + 6)
            .ok_or(ParseError::TooShort)?
            .try_into()
            .expect("slice length matches mac address length"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_three_address_management_action_header() {
            let frame = [
                0xd0, 0x00, // management action
                0x00, 0x00, // duration
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination
                0x02, 0x11, 0x22, 0x33, 0x44, 0x55, // source
                0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, // bssid
                0x10, 0x00, // sequence control
                0x7f, 0x50, // body
            ];

            let parsed = parse_mac_header(&frame).expect("valid 802.11 header");

            assert_eq!(parsed.frame_control, 0x00d0);
            assert_eq!(parsed.duration_id, 0);
            assert_eq!(parsed.destination, [0xff; 6]);
            assert_eq!(parsed.source, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
            assert_eq!(parsed.bssid, [0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]);
            assert_eq!(parsed.sequence_control, 0x0010);
            assert_eq!(parsed.body, &[0x7f, 0x50]);
        }

        #[test]
        fn parses_qos_data_body_after_two_byte_qos_control() {
            // fc = 0x0888 (data + QoS data subtype). owl strips the 2-byte QoS
            // control so the LLC body starts at offset 26, not 24.
            let frame = [
                0x88, 0x08, // frame control (QoS data)
                0x00, 0x00, // duration
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination
                0x02, 0x11, 0x22, 0x33, 0x44, 0x55, // source
                0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, // bssid
                0x10, 0x00, // sequence control
                0x00, 0x00, // QoS control (stripped from body)
                0xaa, 0xaa, 0x03, // LLC/SNAP start
            ];

            let parsed = parse_mac_header(&frame).expect("valid QoS data header");

            assert_eq!(parsed.frame_control, 0x0888);
            assert!(is_qos_data(parsed.frame_control));
            assert_eq!(parsed.body, &[0xaa, 0xaa, 0x03]);
        }

        #[test]
        fn builds_three_address_management_action_frame() {
            let frame = build_management_action_frame(
                [0xff; 6],
                [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                [0x00, 0x25, 0x00, 0xff, 0x94, 0x73],
                0x0010,
                &[0x7f, 0x00],
            );

            assert_eq!(&frame[0..2], &[0xd0, 0x00]);
            assert_eq!(&frame[4..10], &[0xff; 6]);
            assert_eq!(&frame[10..16], &[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            assert_eq!(&frame[16..22], &[0x00, 0x25, 0x00, 0xff, 0x94, 0x73]);
            assert_eq!(&frame[22..24], &[0x10, 0x00]);
            assert_eq!(&frame[24..], &[0x7f, 0x00]);
        }

        #[test]
        fn builds_three_address_data_frame() {
            let frame = build_data_frame(
                [0x33, 0x33, 0, 0, 0, 1],
                [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                [0x00, 0x25, 0x00, 0xff, 0x94, 0x73],
                0x0020,
                &[0xaa, 0xaa],
            );

            assert_eq!(&frame[0..2], &[0x08, 0x00]);
            assert_eq!(&frame[4..10], &[0x33, 0x33, 0, 0, 0, 1]);
            assert_eq!(&frame[22..24], &[0x20, 0x00]);
            assert_eq!(&frame[24..], &[0xaa, 0xaa]);
        }
    }
}

pub mod awdl {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ActionSubtype {
        Psf,
        Mif,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct ActionFrame<'a> {
        pub subtype: ActionSubtype,
        pub phy_tx: u32,
        pub target_tx: u32,
        pub tlvs: &'a [u8],
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ParseError {
        TooShort,
        InvalidVendorHeader,
        UnsupportedSubtype(u8),
        UnexpectedValue,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct Tlv<'a> {
        pub kind: u8,
        pub value: &'a [u8],
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct SyncParameters {
        pub time_to_next_aw_tu: u16,
        pub aw_counter: u16,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct ElectionParametersV2 {
        pub master_addr: [u8; 6],
        pub sync_addr: [u8; 6],
        pub master_counter: u32,
        pub distance_to_master: u32,
        pub master_metric: u32,
        pub self_metric: u32,
        pub self_counter: u32,
    }

    /// owl `awdl_handle_election_params_tlv` fields (owl/src/rx.c:111-131).
    #[derive(Debug, PartialEq, Eq)]
    pub struct ElectionParameters {
        pub height: u8,
        pub master_addr: [u8; 6],
        pub master_metric: u32,
        pub self_metric: u32,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct ChannelSequence {
        pub encoding: u8,
        pub channels: Vec<u16>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct EthernetFrame {
        pub bytes: Vec<u8>,
    }

    pub fn parse_action_frame(body: &[u8]) -> Result<ActionFrame<'_>, ParseError> {
        if body.len() < 16 {
            return Err(ParseError::TooShort);
        }

        let valid = body[0] == 127
            && body[1..4] == [0x00, 0x17, 0xf2]
            && body[4] == 0x08
            && body[5] == 0x10;
        if !valid {
            return Err(ParseError::InvalidVendorHeader);
        }

        let subtype = match body[6] {
            0 => ActionSubtype::Psf,
            3 => ActionSubtype::Mif,
            other => return Err(ParseError::UnsupportedSubtype(other)),
        };

        Ok(ActionFrame {
            subtype,
            phy_tx: u32::from_le_bytes([body[8], body[9], body[10], body[11]]),
            target_tx: u32::from_le_bytes([body[12], body[13], body[14], body[15]]),
            tlvs: &body[16..],
        })
    }

    pub fn parse_tlvs(bytes: &[u8]) -> Result<Vec<Tlv<'_>>, ParseError> {
        // owl iterates TLVs with read_tlv (owl/src/wire.c:209-222) inside a
        // `while ((len = read_tlv(..)) > 0)` loop (owl/src/rx.c:295): an
        // incomplete *trailing* TLV makes read_tlv return OUT_OF_BOUNDS and the
        // loop simply stops — the already-seen TLVs (and the peer, added
        // earlier) are kept. Mirror that tolerance here: stop cleanly on a
        // short trailing header or value rather than failing the whole frame.
        let mut offset = 0;
        let mut tlvs = Vec::new();
        while offset + 3 <= bytes.len() {
            let kind = bytes[offset];
            let len = u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]) as usize;
            offset += 3;
            let Some(value) = bytes.get(offset..offset + len) else {
                break; // incomplete trailing TLV value
            };
            tlvs.push(Tlv { kind, value });
            offset += len;
        }
        Ok(tlvs)
    }

    pub fn parse_sync_parameters(value: &[u8]) -> Result<SyncParameters, ParseError> {
        let time_to_next_aw_tu = read_le16(value, 1)?;
        let aw_counter = read_le16(value, 29)?;

        Ok(SyncParameters {
            time_to_next_aw_tu,
            aw_counter,
        })
    }

    pub fn parse_election_parameters_v2(value: &[u8]) -> Result<ElectionParametersV2, ParseError> {
        Ok(ElectionParametersV2 {
            master_addr: read_addr(value, 0)?,
            sync_addr: read_addr(value, 6)?,
            master_counter: read_le32(value, 12)?,
            distance_to_master: read_le32(value, 16)?,
            master_metric: read_le32(value, 20)?,
            self_metric: read_le32(value, 24)?,
            self_counter: read_le32(value, 36)?,
        })
    }

    /// owl `awdl_handle_election_params_tlv` reads (owl/src/rx.c:120-125):
    /// distance_to_master @3 (u8), master_addr @5, master_metric @11 (le32),
    /// self_metric @15 (le32).
    pub fn parse_election_parameters(value: &[u8]) -> Result<ElectionParameters, ParseError> {
        Ok(ElectionParameters {
            height: *value.get(3).ok_or(ParseError::TooShort)?,
            master_addr: read_addr(value, 5)?,
            master_metric: read_le32(value, 11)?,
            self_metric: read_le32(value, 15)?,
        })
    }

    /// owl `enum awdl_chan_encoding` (owl/src/channel.h:27-31): the
    /// per-entry byte layout of an AWDL channel-sequence TLV.
    pub const CHAN_ENC_SIMPLE: u8 = 0;
    pub const CHAN_ENC_LEGACY: u8 = 1;
    pub const CHAN_ENC_OPCLASS: u8 = 3;

    /// owl `awdl_chan_encoding_size` (owl/src/channel.c:69-79): the number
    /// of bytes per chanseq entry for a given encoding, or `None` for an
    /// unknown encoding. SIMPLE packs one `chan_num` byte; LEGACY and
    /// OPCLASS both use two bytes (with the channel number in different
    /// positions — see [`chan_num`]).
    pub fn chan_encoding_size(enc: u8) -> Option<usize> {
        match enc {
            CHAN_ENC_SIMPLE => Some(1),
            CHAN_ENC_LEGACY | CHAN_ENC_OPCLASS => Some(2),
            _ => None,
        }
    }

    /// owl `awdl_chan_num` (owl/src/channel.c:56-67): decode the real
    /// Wi-Fi channel number from a chanseq entry's raw bytes under the
    /// given encoding, or `None` for an unknown encoding.
    ///
    /// - SIMPLE (0): one byte, `chan_num = bytes[0]`.
    /// - LEGACY (1): two bytes laid out `[flags, chan_num]`, so the channel
    ///   number is the SECOND byte. filin previously read it as the first,
    ///   which yielded garbage channel numbers (e.g. 174/232/245) on live
    ///   Apple frames and made the radio hop to invalid channels.
    /// - OPCLASS (3): two bytes laid out `[chan_num, opclass]`, so the
    ///   channel number is the FIRST byte.
    pub fn chan_num(enc: u8, bytes: &[u8]) -> Option<u8> {
        match enc {
            CHAN_ENC_SIMPLE => bytes.first().copied(),
            CHAN_ENC_LEGACY => bytes.get(1).copied(),
            CHAN_ENC_OPCLASS => bytes.first().copied(),
            _ => None,
        }
    }

    pub fn parse_channel_sequence(value: &[u8]) -> Result<ChannelSequence, ParseError> {
        let count = *value.first().ok_or(ParseError::TooShort)?;
        let encoding = *value.get(1).ok_or(ParseError::TooShort)?;
        let duplicate_count = *value.get(2).ok_or(ParseError::TooShort)?;
        let step_count = *value.get(3).ok_or(ParseError::TooShort)?;
        let fill_channel = read_le16(value, 4)?;

        if count != 15 || duplicate_count != 0 || step_count != 3 || fill_channel != 0xffff {
            return Err(ParseError::UnexpectedValue);
        }

        // owl awdl_chan_encoding_size (channel.c:69-79). filin previously
        // mapped encoding 1→1 byte and rejected encoding 0, both wrong; the
        // correct sizes are 0→1, 1→2, 3→2 (owl channel.h:27-31).
        let field_len = chan_encoding_size(encoding).ok_or(ParseError::UnexpectedValue)?;
        let mut offset = 6;
        let mut channels = Vec::with_capacity(16);
        for _ in 0..16 {
            let entry = value
                .get(offset..offset + field_len)
                .ok_or(ParseError::TooShort)?;
            // owl awdl_chan_num (channel.c:56-67): decode the REAL channel
            // number per encoding, not a raw little-endian u16 of both
            // bytes (which mixed flags/opclass into the channel and produced
            // invalid channels like 174/232/245 on the live wire).
            let channel = u16::from(chan_num(encoding, entry).ok_or(ParseError::UnexpectedValue)?);
            channels.push(channel);
            offset += field_len;
        }

        Ok(ChannelSequence { encoding, channels })
    }

    pub fn decapsulate_data_frame(
        body: &[u8],
        source: [u8; 6],
        destination: [u8; 6],
    ) -> Result<EthernetFrame, ParseError> {
        if body.len() < 16 {
            return Err(ParseError::TooShort);
        }
        if body[0..3] != [0xaa, 0xaa, 0x03] || body[6..8] != [0x08, 0x00] {
            return Err(ParseError::UnexpectedValue);
        }

        let ethertype = [body[14], body[15]];
        let payload = &body[16..];
        let mut bytes = Vec::with_capacity(14 + payload.len());
        bytes.extend_from_slice(&destination);
        bytes.extend_from_slice(&source);
        bytes.extend_from_slice(&ethertype);
        bytes.extend_from_slice(payload);
        Ok(EthernetFrame { bytes })
    }

    /// owl `awdl_rx_data_amsdu` (owl/src/rx.c:391-418): iterates A-MSDU
    /// subframes. Each subframe has a 14-byte Ethernet-like header
    /// (dst, src, length-be), then `length` bytes of LLC/SNAP+AWDL-data, then
    /// padding to a 4-byte boundary.
    pub fn decapsulate_amsdu_data_frames(body: &[u8]) -> Result<Vec<EthernetFrame>, ParseError> {
        let mut out = Vec::new();
        let mut offset = 0;
        while offset < body.len() {
            if offset + 14 > body.len() {
                return Err(ParseError::TooShort);
            }
            let dst: [u8; 6] = body[offset..offset + 6].try_into().unwrap();
            let src: [u8; 6] = body[offset + 6..offset + 12].try_into().unwrap();
            let sub_len = u16::from_be_bytes([body[offset + 12], body[offset + 13]]) as usize;
            offset += 14;
            if offset + sub_len > body.len() {
                return Err(ParseError::TooShort);
            }
            let sub_body = &body[offset..offset + sub_len];
            offset += sub_len;
            // strip padding to 4-byte boundary (owl/src/rx.c:413)
            if offset < body.len() {
                offset += (4 - ((14 + sub_len) % 4)) % 4;
            }
            match decapsulate_data_frame(sub_body, src, dst) {
                Ok(frame) => out.push(frame),
                Err(err) => return Err(err),
            }
        }
        Ok(out)
    }

    pub fn build_action_body(
        subtype: ActionSubtype,
        phy_tx: u32,
        target_tx: u32,
        tlvs: &[u8],
    ) -> Vec<u8> {
        let subtype = match subtype {
            ActionSubtype::Psf => 0,
            ActionSubtype::Mif => 3,
        };
        let mut body = Vec::with_capacity(16 + tlvs.len());
        body.extend_from_slice(&[0x7f, 0x00, 0x17, 0xf2, 0x08, 0x10, subtype, 0x00]);
        body.extend_from_slice(&phy_tx.to_le_bytes());
        body.extend_from_slice(&target_tx.to_le_bytes());
        body.extend_from_slice(tlvs);
        body
    }

    pub fn encapsulate_ethernet_payload(ethernet: &[u8], seq: u16) -> Result<Vec<u8>, ParseError> {
        if ethernet.len() < 14 {
            return Err(ParseError::TooShort);
        }
        let ethertype = [ethernet[12], ethernet[13]];
        let payload = &ethernet[14..];
        let mut body = Vec::with_capacity(16 + payload.len());
        body.extend_from_slice(&[0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2, 0x08, 0x00]);
        body.extend_from_slice(&0x0403u16.to_le_bytes()); // AWDL_DATA_HEAD (owl/src/tx.c:33)
        body.extend_from_slice(&seq.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&ethertype);
        body.extend_from_slice(payload);
        Ok(body)
    }

    fn read_le16(bytes: &[u8], offset: usize) -> Result<u16, ParseError> {
        let field = bytes.get(offset..offset + 2).ok_or(ParseError::TooShort)?;
        Ok(u16::from_le_bytes([field[0], field[1]]))
    }

    fn read_le32(bytes: &[u8], offset: usize) -> Result<u32, ParseError> {
        let field = bytes.get(offset..offset + 4).ok_or(ParseError::TooShort)?;
        Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
    }

    fn read_addr(bytes: &[u8], offset: usize) -> Result<[u8; 6], ParseError> {
        Ok(bytes
            .get(offset..offset + 6)
            .ok_or(ParseError::TooShort)?
            .try_into()
            .expect("slice length matches mac address length"))
    }

    /// Derive a peer's link-local IPv6 address from its MAC using the RFC 4291
    /// modified EUI-64 method (the AWDL addressing scheme; see
    /// docs/book/07-service-discovery-addressing.md and owl's
    /// `rfc4291_addr`, owl/daemon/netutils.c:607-619). AWDL does NOT use NDP,
    /// so filin must populate the system neighbor table with these mappings to
    /// make unicast awdl0 connectivity work (the AirDrop HTTPS /Discover
    /// handshake — FILIN_NEIGHBOR_TABLE.md).
    ///
    /// `fe80::(mac0 ^ 0x02):mac1:mac2:ff:fe:mac3:mac4:mac5`.
    pub fn link_local_ipv6(mac: [u8; 6]) -> [u8; 16] {
        let mut v6 = [0u8; 16];
        v6[0] = 0xfe;
        v6[1] = 0x80;
        // bytes 2..8 stay zero (fe80::/64 prefix)
        v6[8] = mac[0] ^ 0x02; // flip the Universal/Local bit
        v6[9] = mac[1];
        v6[10] = mac[2];
        v6[11] = 0xff; // EUI-64 middle marker
        v6[12] = 0xfe;
        v6[13] = mac[3];
        v6[14] = mac[4];
        v6[15] = mac[5];
        v6
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_psf_vendor_action_header() {
            let body = [
                0x7f, // vendor-specific action category
                0x00, 0x17, 0xf2, // Apple OUI
                0x08, // AWDL type
                0x10, // AWDL version 1.0
                0x00, // PSF subtype
                0x00, // reserved
                0x44, 0x33, 0x22, 0x11, // phy_tx
                0x88, 0x77, 0x66, 0x55, // target_tx
                0x04, 0x02, 0x00, 0xaa, 0xbb, // first TLV
            ];

            let parsed = parse_action_frame(&body).expect("valid AWDL PSF action frame");

            assert_eq!(parsed.subtype, ActionSubtype::Psf);
            assert_eq!(parsed.phy_tx, 0x1122_3344);
            assert_eq!(parsed.target_tx, 0x5566_7788);
            assert_eq!(parsed.tlvs, &[0x04, 0x02, 0x00, 0xaa, 0xbb]);
        }

        #[test]
        fn parses_repeated_tlvs() {
            let tlvs = [
                0x04, 0x02, 0x00, 0xaa, 0xbb, // sync params fragment
                0x18, 0x04, 0x00, 0x01, 0x02, 0x03, 0x04, // election v2 fragment
            ];

            let parsed = parse_tlvs(&tlvs).expect("valid AWDL TLVs");

            assert_eq!(
                parsed,
                vec![
                    Tlv {
                        kind: 0x04,
                        value: &[0xaa, 0xbb]
                    },
                    Tlv {
                        kind: 0x18,
                        value: &[0x01, 0x02, 0x03, 0x04]
                    }
                ]
            );
        }

        #[test]
        fn parse_tlvs_stops_cleanly_on_incomplete_trailing_tlv() {
            // Two good TLVs followed by an incomplete trailing value, then a
            // short header fragment — owl's read_tlv loop exits on both.
            let tlvs = [
                0x04, 0x02, 0x00, 0xaa, 0xbb, // ok: sync params fragment
                0x18, 0x04, 0x00, 0x01, 0x02, 0x03, 0x04, // ok: election v2 fragment
                0x07, 0x08, 0x00, 0xce, 0x11, // incomplete: claims len 8 but only 2 bytes
                0xde, 0xad, // dangling header fragment (< 3 bytes)
            ];

            let parsed = parse_tlvs(&tlvs).expect("tolerant parse keeps leading TLVs");

            assert_eq!(parsed.len(), 2);
            assert_eq!(parsed[0].kind, 0x04);
            assert_eq!(parsed[1].kind, 0x18);
        }

        #[test]
        fn parses_sync_parameters_tlv_timing_fields() {
            let mut value = [0u8; 31];
            value[1..3].copy_from_slice(&0x1234u16.to_le_bytes());
            value[29..31].copy_from_slice(&0x5678u16.to_le_bytes());

            let parsed = parse_sync_parameters(&value).expect("valid sync parameters TLV");

            assert_eq!(
                parsed,
                SyncParameters {
                    time_to_next_aw_tu: 0x1234,
                    aw_counter: 0x5678
                }
            );
        }

        #[test]
        fn parses_election_parameters_v2_tlv() {
            let mut value = [0u8; 40];
            value[0..6].copy_from_slice(&[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            value[6..12].copy_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
            value[12..16].copy_from_slice(&10u32.to_le_bytes());
            value[16..20].copy_from_slice(&2u32.to_le_bytes());
            value[20..24].copy_from_slice(&900u32.to_le_bytes());
            value[24..28].copy_from_slice(&800u32.to_le_bytes());
            value[36..40].copy_from_slice(&7u32.to_le_bytes());

            let parsed =
                parse_election_parameters_v2(&value).expect("valid election parameters v2 TLV");

            assert_eq!(
                parsed,
                ElectionParametersV2 {
                    master_addr: [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                    sync_addr: [0x02, 0x11, 0x22, 0x33, 0x44, 0x55],
                    master_counter: 10,
                    distance_to_master: 2,
                    master_metric: 900,
                    self_metric: 800,
                    self_counter: 7,
                }
            );
        }

        #[test]
        fn parses_election_parameters_v1_tlv() {
            // layout per owl/src/rx.c:120-125: flags@0, id@1-2, distancetop@3,
            // unknown@4, master_addr@5, master_metric@11, self_metric@15, pad@19
            let mut value = [0u8; 21];
            value[3] = 4; // distance_to_master -> height
            value[5..11].copy_from_slice(&[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            value[11..15].copy_from_slice(&900u32.to_le_bytes());
            value[15..19].copy_from_slice(&800u32.to_le_bytes());

            let parsed =
                parse_election_parameters(&value).expect("valid election parameters v1 TLV");

            assert_eq!(
                parsed,
                ElectionParameters {
                    height: 4,
                    master_addr: [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                    master_metric: 900,
                    self_metric: 800,
                }
            );
        }

        #[test]
        fn rejects_unknown_chanseq_encoding() {
            // owl defines only encodings 0, 1, 3 (channel.h:27-31). The old
            // filin parser accepted a non-existent encoding 2; it must now be
            // rejected so a malformed/unknown TLV can't misdecode channels.
            let mut value = vec![15, 2, 0, 3, 0xff, 0xff];
            for channel in 1u16..=16 {
                value.extend_from_slice(&channel.to_le_bytes());
            }
            assert_eq!(
                parse_channel_sequence(&value).unwrap_err(),
                ParseError::UnexpectedValue
            );
        }

        /// Build a chanseq TLV body with a fixed 6-byte header and 16 entries
        /// laid out per `encoding`, using `entry_bytes(i)` for slot i.
        fn chanseq_tlv<F: Fn(usize) -> Vec<u8>>(encoding: u8, entry_bytes: F) -> Vec<u8> {
            let mut value = vec![15, encoding, 0, 3, 0xff, 0xff];
            for i in 0..16 {
                value.extend(entry_bytes(i));
            }
            value
        }

        #[test]
        fn parses_opclass_channel_sequence_tlv() {
            // AWDL_CHAN_ENC_OPCLASS (3): two bytes [chan_num, opclass].
            // Apple's default chanseq (owl awdl_chanseq_init) is 8x149 then
            // 8x6 — exactly the kind of sequence filin must hop through.
            let value = chanseq_tlv(CHAN_ENC_OPCLASS, |i| {
                let ch = if i < 8 { 149 } else { 6 };
                vec![ch, 0x80]
            });
            let parsed =
                parse_channel_sequence(&value).expect("valid OPCLASS channel sequence TLV");
            assert_eq!(parsed.encoding, CHAN_ENC_OPCLASS);
            // Real channel numbers, not the raw opclass-or-u16 garbage.
            assert_eq!(&parsed.channels[0..8], &[149; 8]);
            assert_eq!(&parsed.channels[8..16], &[6; 8]);
        }

        #[test]
        fn parses_legacy_channel_sequence_tlv() {
            // AWDL_CHAN_ENC_LEGACY (1): two bytes [flags, chan_num], so the
            // channel number is the SECOND byte. filin previously read the
            // first byte (flags) as the channel, which produced the live
            // garbage channels (174/232/245…). This is the regression test
            // for the live iPhone upload stall.
            let value = chanseq_tlv(CHAN_ENC_LEGACY, |i| {
                let ch = if i < 8 { 44 } else { 149 };
                // flags byte is non-zero to prove we do not return it.
                vec![0xa4, ch]
            });
            let parsed =
                parse_channel_sequence(&value).expect("valid LEGACY channel sequence TLV");
            assert_eq!(parsed.encoding, CHAN_ENC_LEGACY);
            assert_eq!(&parsed.channels[0..8], &[44; 8]);
            assert_eq!(&parsed.channels[8..16], &[149; 8]);
        }

        #[test]
        fn parses_simple_channel_sequence_tlv() {
            // AWDL_CHAN_ENC_SIMPLE (0): one byte = chan_num. filin previously
            // rejected encoding 0 outright; owl accepts it.
            let value = chanseq_tlv(CHAN_ENC_SIMPLE, |i| {
                vec![if i < 8 { 44u8 } else { 6u8 }]
            });
            let parsed =
                parse_channel_sequence(&value).expect("valid SIMPLE channel sequence TLV");
            assert_eq!(parsed.encoding, CHAN_ENC_SIMPLE);
            assert_eq!(&parsed.channels[0..8], &[44u16; 8]);
            assert_eq!(&parsed.channels[8..16], &[6u16; 8]);
        }

        #[test]
        fn chan_num_decodes_per_encoding() {
            // owl awdl_chan_num (channel.c:56-67) for each encoding.
            assert_eq!(chan_num(CHAN_ENC_SIMPLE, &[44]), Some(44));
            assert_eq!(chan_num(CHAN_ENC_LEGACY, &[0xa4, 44]), Some(44));
            assert_eq!(chan_num(CHAN_ENC_OPCLASS, &[44, 0x80]), Some(44));
            assert_eq!(chan_num(9, &[44]), None);
        }

        #[test]
        fn chan_encoding_size_matches_owl() {
            assert_eq!(chan_encoding_size(CHAN_ENC_SIMPLE), Some(1));
            assert_eq!(chan_encoding_size(CHAN_ENC_LEGACY), Some(2));
            assert_eq!(chan_encoding_size(CHAN_ENC_OPCLASS), Some(2));
            assert_eq!(chan_encoding_size(9), None);
        }

        #[test]
        fn decapsulates_awdl_data_body_to_ethernet_frame() {
            let mut body = vec![
                0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2, 0x08, 0x00, // LLC/SNAP
                0x00, 0x00, // head
                0x01, 0x00, // seq
                0x00, 0x00, // pad
                0x86, 0xdd, // ethertype IPv6
            ];
            body.extend_from_slice(&[0x60, 0x00, 0x00, 0x00]);

            let frame = decapsulate_data_frame(
                &body,
                [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                [0x33, 0x33, 0x00, 0x00, 0x00, 0x01],
            )
            .expect("valid AWDL data frame");

            assert_eq!(
                frame.bytes,
                vec![
                    0x33, 0x33, 0x00, 0x00, 0x00, 0x01, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x86,
                    0xdd, 0x60, 0x00, 0x00, 0x00,
                ]
            );
        }

        #[test]
        fn decapsulates_amsdu_with_two_subframes() {
            // build two LLC/SNAP+AWDL-data subframes
            let sub1 = vec![
                0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2, 0x08, 0x00, // LLC/SNAP
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // AWDL data header
                0x86, 0xdd, // ethertype
                0x60, 0x00, 0x00, 0x00, // payload
            ];
            let sub2 = vec![
                0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2, 0x08, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x86, 0xdd,
                0x60, 0x00, 0x00, 0x01,
            ];
            let dst1 = [0x33, 0x33, 0x00, 0x00, 0x00, 0x01u8];
            let src1 = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            let dst2 = [0x33, 0x33, 0x00, 0x00, 0x00, 0x02];
            let src2 = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xff];

            // A-MSDU body = subframe1-header + sub1 + padding + subframe2-header + sub2
            let mut body = Vec::new();
            body.extend_from_slice(&dst1);
            body.extend_from_slice(&src1);
            body.extend_from_slice(&(sub1.len() as u16).to_be_bytes());
            body.extend_from_slice(&sub1);
            // pad to 4-byte boundary: (14 + sub1.len()) % 4
            let pad1 = (4 - ((14 + sub1.len()) % 4)) % 4;
            body.extend(std::iter::repeat_n(0u8, pad1));
            body.extend_from_slice(&dst2);
            body.extend_from_slice(&src2);
            body.extend_from_slice(&(sub2.len() as u16).to_be_bytes());
            body.extend_from_slice(&sub2);

            let frames = decapsulate_amsdu_data_frames(&body).expect("valid A-MSDU");
            assert_eq!(frames.len(), 2);
            assert_eq!(&frames[0].bytes[0..6], &dst1);
            assert_eq!(&frames[0].bytes[6..12], &src1);
            assert_eq!(&frames[1].bytes[0..6], &dst2);
        }

        #[test]
        fn builds_mif_vendor_action_body() {
            let body =
                build_action_body(ActionSubtype::Mif, 0x1122_3344, 0x5566_7788, &[0x04, 0, 0]);

            assert_eq!(
                body,
                vec![
                    0x7f, 0x00, 0x17, 0xf2, 0x08, 0x10, 0x03, 0x00, 0x44, 0x33, 0x22, 0x11, 0x88,
                    0x77, 0x66, 0x55, 0x04, 0x00, 0x00,
                ]
            );
        }

        #[test]
        fn encapsulates_ethernet_payload_as_awdl_data_body() {
            let ethernet = [
                0x33, 0x33, 0x00, 0x00, 0x00, 0x01, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x86, 0xdd,
                0x60, 0x00,
            ];

            let body = encapsulate_ethernet_payload(&ethernet, 7).expect("valid ethernet frame");

            // owl AWDL_DATA_HEAD = 0x0403 (owl/src/tx.c:33, frame.h:37). A real
            // Apple peer may reject data frames with head=0; filin's own decap
            // ignores the field so loopback tests won't catch this.
            assert_eq!(
                body,
                vec![
                    0xaa, 0xaa, 0x03, 0x00, 0x17, 0xf2, 0x08, 0x00,
                    0x03, 0x04, // head = AWDL_DATA_HEAD = 0x0403 little-endian
                    0x07, 0x00, // seq
                    0x00, 0x00, // reserved
                    0x86, 0xdd, // ethertype
                    0x60, 0x00, // payload
                ]
            );
        }

        // --- RFC 4291 modified EUI-64 link-local IPv6 derivation
        //     (FILIN_NEIGHBOR_TABLE.md: owl rfc4291_addr, netutils.c:607). ---

        #[test]
        fn link_local_ipv6_known_vector_mac_to_ipv6() {
            // RFC 4291 Appendix A example: MAC 00:90:27:3a:db:32 →
            // fe80::290:27ff:fe3a:db32. The modified EUI-64 flips the U/L bit
            // (bit 1 of byte 0): 0x00 ^ 0x02 = 0x02.
            let v6 = link_local_ipv6([0x00, 0x90, 0x27, 0x3a, 0xdb, 0x32]);
            assert_eq!(
                v6,
                [
                    0xfe, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x90, 0x27, 0xff, 0xfe,
                    0x3a, 0xdb, 0x32
                ]
            );
        }

        #[test]
        fn link_local_ipv6_flips_universal_local_bit() {
            // The only transformation is flipping bit 1 of byte 0 (U/L bit).
            // A MAC with that bit already set (locally administered) has it
            // cleared: 0x02 ^ 0x02 = 0x00.
            let v6 = link_local_ipv6([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            assert_eq!(
                v6,
                [
                    0xfe, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xbb, 0xff, 0xfe,
                    0xcc, 0xdd, 0xee
                ]
            );
        }

        #[test]
        fn link_local_ipv6_round_trips_arbitrary_macs() {
            for mac in [
                [0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                [0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
                [0xdc, 0x56, 0xe7, 0xc1, 0x9a, 0x3b],
                [0xa4, 0x83, 0xe7, 0x44, 0xc1, 0x82],
            ] {
                let v6 = link_local_ipv6(mac);
                // Prefix is always fe80::/64.
                assert_eq!(v6[0], 0xfe);
                assert_eq!(v6[1], 0x80);
                assert!(v6[2..8].iter().all(|&b| b == 0));
                // Byte 8 = mac[0] ^ 0x02 (U/L bit flipped).
                assert_eq!(v6[8], mac[0] ^ 0x02);
                // Bytes 9-10 = mac[1-2] verbatim.
                assert_eq!(v6[9], mac[1]);
                assert_eq!(v6[10], mac[2]);
                // Bytes 11-12 = ff:fe (the EUI-64 middle marker).
                assert_eq!(v6[11], 0xff);
                assert_eq!(v6[12], 0xfe);
                // Bytes 13-15 = mac[3-5] verbatim.
                assert_eq!(v6[13], mac[3]);
                assert_eq!(v6[14], mac[4]);
                assert_eq!(v6[15], mac[5]);
            }
        }
    }
}

pub mod election {
    //! Faithful port of owl `election.c` / `election.h`.
    //!
    //! AWDL master election: each node advertises an election state and adopts
    //! the peer with the best `(master_counter, master_metric)`, breaking ties
    //! by shorter sync tree (height) then larger address. A node never syncs to
    //! a peer that already syncs to it (cycle prevention) and rejects trees
    //! taller than `TREE_MAX_HEIGHT`.

    /// owl `AWDL_ELECTION_METRIC_INIT` (owl/src/election.h:26).
    pub const METRIC_INIT: u32 = 60;
    /// owl `AWDL_ELECTION_COUNTER_INIT` (owl/src/election.h:27).
    pub const COUNTER_INIT: u32 = 0;
    /// owl `AWDL_ELECTION_TREE_MAX_HEIGHT` (owl/src/election.h:25).
    pub const TREE_MAX_HEIGHT: u32 = 10;

    /// Default `(self_counter, self_metric)` advertised in `--force-master`
    /// mode. High enough to beat any real Apple master's election parameters
    /// so peers adopt filin and follow its (all-social-channel) sequence, but
    /// deliberately below `u32::MAX` so it still looks like a plausible metric
    /// rather than an obvious sentinel. Crank it via `--force-master-metric`
    /// if a cluster refuses to switch.
    pub const DEFAULT_FORCE_MASTER_METRIC: u32 = 0x4000_0000;

    /// owl `struct awdl_election_state` (owl/src/election.h:31-40).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ElectionState {
        pub master_addr: [u8; 6],
        pub sync_addr: [u8; 6],
        pub self_addr: [u8; 6],
        pub height: u32,
        pub master_metric: u32,
        pub self_metric: u32,
        pub master_counter: u32,
        pub self_counter: u32,
        /// `--force-master`: when set, [`run`](ElectionState::run) never adopts
        /// a peer, so filin stays its own master (`master_addr == self_addr`,
        /// `height == 0`) and advertises its own (inflated) election params.
        /// This is what forces a DFS/No-IR-anchored Apple cluster onto filin's
        /// social-channel sequence (see [`force_self_master`]).
        pub force_master: bool,
    }

    impl ElectionState {
        /// owl `awdl_election_state_init` (owl/src/election.c:52-58).
        pub fn new(self_addr: [u8; 6]) -> Self {
            Self {
                master_addr: self_addr,
                sync_addr: self_addr,
                self_addr,
                height: 0,
                master_metric: METRIC_INIT,
                self_metric: METRIC_INIT,
                master_counter: COUNTER_INIT,
                self_counter: COUNTER_INIT,
                force_master: false,
            }
        }

        /// Enable `--force-master`: advertise an inflated `(self_counter,
        /// self_metric)` and refuse to adopt any peer, so filin becomes — and
        /// stays — the cluster's elected master. Peers that hear filin's
        /// announce adopt it and follow filin's channel sequence (which, with a
        /// non-DFS anchor, is all social channels), pulling them off any
        /// No-IR/DFS channel the previous master had anchored on. `metric` is
        /// used for both the counter (election primary key) and the metric.
        pub fn force_self_master(&mut self, metric: u32) {
            self.force_master = true;
            self.self_metric = metric;
            self.self_counter = metric;
            // Reflect immediately so an announce sent before the first `run`
            // already carries the winning params with self as master.
            self.master_addr = self.self_addr;
            self.sync_addr = self.self_addr;
            self.height = 0;
            self.master_metric = metric;
            self.master_counter = metric;
        }

        /// owl `awdl_election_is_sync_master` (owl/src/election.c:35-37).
        pub fn is_sync_master(&self, addr: &[u8; 6]) -> bool {
            self.sync_addr == *addr
        }

        /// owl `awdl_election_run` (owl/src/election.c:67-120). Picks the best
        /// master from the supplied peer election views and updates self in
        /// place. `peers` need not be sorted; iteration order matches owl's.
        pub fn run(&mut self, peers: &[PeerElection]) {
            self.reset_self();

            // `--force-master`: reset_self() has restored self as master with
            // the inflated params; skip adoption entirely so no peer (even one
            // advertising a higher counter) can displace us.
            if self.force_master {
                return;
            }

            // `master_state` starts as self (owl/src/election.c:71).
            let mut best_counter = self.self_counter;
            let mut best_metric = self.self_metric;
            let mut best_height = 0u32;
            let mut best_addr = self.self_addr;
            let mut adopted: Option<&PeerElection> = None;

            for peer in peers {
                if !peer.is_valid {
                    continue;
                }
                if peer.height + 1 > TREE_MAX_HEIGHT {
                    continue;
                }
                if peer.sync_addr == self.self_addr {
                    continue; // reject: would form a cycle in the sync tree
                }
                // NOTE: a master anchored on a No-IR/DFS channel is still
                // adopted here — we follow it for AW *timing*. We just never
                // tune the monitor to its No-IR slots: sync_channel_sequence
                // rewrites those to TX-capable social channels so our announce
                // lands in-window on 44/149/6 (the No-IR channels are where we
                // could neither TX nor usefully RX anyway).
                let cmp = (peer.master_counter, peer.master_metric)
                    .cmp(&(best_counter, best_metric));
                match cmp {
                    std::cmp::Ordering::Less => continue, // reject: lower master
                    std::cmp::Ordering::Equal => {
                        if peer.height > best_height {
                            continue; // reject: longer sync tree
                        } else if peer.height == best_height && peer.self_addr <= best_addr {
                            continue; // reject: smaller/equal address tie-break
                        }
                    }
                    std::cmp::Ordering::Greater => {}
                }
                // accept
                adopted = Some(peer);
                best_counter = peer.master_counter;
                best_metric = peer.master_metric;
                best_height = peer.height;
                best_addr = peer.self_addr;
            }

            if let Some(peer) = adopted {
                self.master_addr = peer.master_addr;
                self.sync_addr = peer.self_addr;
                self.master_metric = peer.master_metric;
                self.master_counter = peer.master_counter;
                self.height = peer.height + 1;
            }
        }

        /// owl `awdl_election_reset_self` (owl/src/election.c:44-50).
        fn reset_self(&mut self) {
            self.height = 0;
            self.master_addr = self.self_addr;
            self.sync_addr = self.self_addr;
            self.master_metric = self.self_metric;
            self.master_counter = self.self_counter;
        }
    }

    /// Read-only view of a peer's election state consumed by `ElectionState::run`.
    /// Mirrors the fields owl reads from `struct awdl_peer` during election.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PeerElection {
        pub self_addr: [u8; 6],
        pub sync_addr: [u8; 6],
        pub master_addr: [u8; 6],
        pub height: u32,
        pub master_metric: u32,
        pub master_counter: u32,
        pub is_valid: bool,
        /// The peer's chanseq anchor (slot-0 primary) channel. Used to reject
        /// masters anchored on a No-IR channel we cannot transmit on. 0 = no
        /// decoded sequence yet (treated as eligible).
        pub anchor_channel: u8,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn local(addr: [u8; 6]) -> ElectionState {
            ElectionState::new(addr)
        }

        /// A peer view that syncs to itself (is its own master) with the given
        /// advertised master counter/metric.
        fn peer_master(
            self_addr: [u8; 6],
            master_counter: u32,
            master_metric: u32,
            height: u32,
        ) -> PeerElection {
            PeerElection {
                self_addr,
                sync_addr: self_addr,
                master_addr: self_addr,
                height,
                master_metric,
                master_counter,
                is_valid: true,
                anchor_channel: 0,
            }
        }

        /// Like `peer_master` but with an explicit chanseq anchor channel, for
        /// testing the No-IR master rejection.
        fn peer_master_on_channel(
            self_addr: [u8; 6],
            master_counter: u32,
            master_metric: u32,
            height: u32,
            anchor_channel: u8,
        ) -> PeerElection {
            PeerElection {
                anchor_channel,
                ..peer_master(self_addr, master_counter, master_metric, height)
            }
        }

        #[test]
        fn lone_node_is_self_master_with_init_values() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            state.run(&[]);
            assert_eq!(state.master_addr, [0x02, 0, 0, 0, 0, 1]);
            assert_eq!(state.sync_addr, [0x02, 0, 0, 0, 0, 1]);
            assert_eq!(state.master_metric, METRIC_INIT);
            assert_eq!(state.master_counter, COUNTER_INIT);
            assert_eq!(state.height, 0);
        }

        #[test]
        fn adopts_peer_with_higher_master_counter() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let peer = peer_master([0x02, 0, 0, 0, 0, 2], 10, 200, 0);

            state.run(std::slice::from_ref(&peer));

            assert_eq!(state.master_addr, [0x02, 0, 0, 0, 0, 2]);
            assert_eq!(state.sync_addr, [0x02, 0, 0, 0, 0, 2]);
            assert_eq!(state.master_metric, 200);
            assert_eq!(state.master_counter, 10);
            assert_eq!(state.height, 1);
        }

        #[test]
        fn force_master_never_adopts_even_higher_counter_peer() {
            // --force-master: a peer advertising a far higher (counter, metric)
            // than our inflated values must NOT displace us. filin stays its
            // own master at height 0 so it imposes its own channel sequence.
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            state.force_self_master(DEFAULT_FORCE_MASTER_METRIC);
            let strong_peer = peer_master([0x02, 0xce, 0x47, 0, 0, 0], u32::MAX, u32::MAX, 0);

            state.run(std::slice::from_ref(&strong_peer));

            assert_eq!(state.master_addr, [0x02, 0, 0, 0, 0, 1]); // self
            assert_eq!(state.sync_addr, [0x02, 0, 0, 0, 0, 1]); // self
            assert_eq!(state.height, 0);
            assert_eq!(state.master_counter, DEFAULT_FORCE_MASTER_METRIC);
            assert_eq!(state.master_metric, DEFAULT_FORCE_MASTER_METRIC);
        }

        #[test]
        fn force_master_advertises_winning_params_before_first_run() {
            // The announce builder reads master_* directly; force_self_master
            // must populate them immediately (an announce can go out before the
            // first election run).
            let mut state = local([0x02, 0, 0, 0, 0, 7]);
            state.force_self_master(0x1234_5678);
            assert_eq!(state.master_addr, [0x02, 0, 0, 0, 0, 7]);
            assert_eq!(state.master_counter, 0x1234_5678);
            assert_eq!(state.master_metric, 0x1234_5678);
            assert_eq!(state.self_counter, 0x1234_5678);
            assert_eq!(state.self_metric, 0x1234_5678);
        }

        #[test]
        fn adopts_no_ir_anchored_master_for_timing() {
            // A No-IR/DFS-anchored master (ce:47 on ch52) IS adopted — we
            // follow it for AW timing. The No-IR slots are rewritten to
            // TX-capable social channels later (sync_channel_sequence /
            // schedule::replace_no_ir_slots), not rejected at election time.
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let no_ir = peer_master_on_channel([0x02, 0xce, 0x47, 0, 0, 0], 100, 5000, 0, 52);
            state.run(std::slice::from_ref(&no_ir));
            assert_eq!(state.sync_addr, [0x02, 0xce, 0x47, 0, 0, 0]); // adopted
        }

        #[test]
        fn rejects_peer_with_lower_master_than_self() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            // self advertises (counter=0, metric=60); peer advertises metric 10 < 60
            let peer = peer_master([0x02, 0, 0, 0, 0, 2], 0, 10, 0);

            state.run(std::slice::from_ref(&peer));

            assert_eq!(state.sync_addr, state.self_addr); // still self-master
        }

        #[test]
        fn breaks_equal_master_tie_by_shorter_height_then_address() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let tall = peer_master([0x02, 0, 0, 0, 0, 9], 5, 100, 3);
            let short_same = peer_master([0x02, 0, 0, 0, 0, 2], 5, 100, 0);
            let short_larger = peer_master([0x02, 0, 0, 0, 0, 3], 5, 100, 0);

            state.run(&[tall, short_same, short_larger]);

            // equal (counter,metric); short_larger wins on address tie-break
            assert_eq!(state.sync_addr, [0x02, 0, 0, 0, 0, 3]);
        }

        #[test]
        fn rejects_peer_that_syncs_to_self_no_cycles() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            // peer claims to sync to us -> would create a cycle
            let mut peer = peer_master([0x02, 0, 0, 0, 0, 2], 99, 9999, 0);
            peer.sync_addr = [0x02, 0, 0, 0, 0, 1];

            state.run(std::slice::from_ref(&peer));

            assert_eq!(state.sync_addr, state.self_addr); // rejected
        }

        #[test]
        fn rejects_peer_whose_tree_would_exceed_max_height() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let peer = peer_master([0x02, 0, 0, 0, 0, 2], 99, 9999, TREE_MAX_HEIGHT);

            state.run(std::slice::from_ref(&peer));

            assert_eq!(state.sync_addr, state.self_addr); // rejected
        }

        #[test]
        fn ignores_invalid_peers() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let mut peer = peer_master([0x02, 0, 0, 0, 0, 2], 99, 9999, 0);
            peer.is_valid = false;

            state.run(std::slice::from_ref(&peer));

            assert_eq!(state.sync_addr, state.self_addr); // ignored
        }

        #[test]
        fn is_sync_master_detects_elected_master() {
            let mut state = local([0x02, 0, 0, 0, 0, 1]);
            let peer_addr = [0x02, 0, 0, 0, 0, 2];
            state.run(std::slice::from_ref(&peer_master(peer_addr, 1, 200, 0)));

            assert!(state.is_sync_master(&peer_addr));
            assert!(!state.is_sync_master(&[0x02, 0, 0, 0, 0, 9]));
        }
    }
}

pub mod sync {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SyncState {
        pub last_update_us: u64,
        pub aw_counter: u16,
        pub aw_period_tu: u16,
        pub presence_mode: u16,
    }

    impl SyncState {
        pub fn new(now_us: u64) -> Self {
            Self {
                last_update_us: now_us,
                aw_counter: 0,
                aw_period_tu: 16,
                presence_mode: 4,
            }
        }

        pub fn update_from_master(
            &mut self,
            rx_hw_time_us: u64,
            time_to_next_aw_tu: u16,
            aw_counter: u16,
        ) {
            let eaw_period_tu = u64::from(self.presence_mode) * u64::from(self.aw_period_tu);
            self.last_update_us = rx_hw_time_us
                .saturating_sub(tu_to_us(eaw_period_tu - u64::from(time_to_next_aw_tu)));
            self.aw_counter = aw_counter & 0xfffc;
        }

        pub fn sync_error_tu(&self, now_us: u64, time_to_next_aw_tu: u16, aw_counter: u16) -> i64 {
            let master_delta = (u64::from(aw_counter) / u64::from(self.presence_mode)) as i64
                - i64::from(self.current_eaw(now_us));
            let aw_delta = i64::from(time_to_next_aw_tu) - i64::from(self.next_aw_tu(now_us));
            master_delta * i64::from(self.presence_mode) * i64::from(self.aw_period_tu) - aw_delta
        }

        pub fn next_aw_tu(&self, now_us: u64) -> u16 {
            let eaw_period_tu = u64::from(self.presence_mode) * u64::from(self.aw_period_tu);
            let time_since_tu = us_to_tu(now_us.saturating_sub(self.last_update_us));
            (eaw_period_tu - (time_since_tu % eaw_period_tu)) as u16
        }

        /// owl `awdl_sync_next_aw_us` (owl/src/sync.c:40-45): microseconds to
        /// the next availability-window boundary.
        pub fn next_aw_us(&self, now_us: u64) -> u64 {
            let eaw_period_us = tu_to_us(
                u64::from(self.presence_mode) * u64::from(self.aw_period_tu),
            );
            let time_since = now_us.saturating_sub(self.last_update_us);
            eaw_period_us - (time_since % eaw_period_us)
        }

        pub fn current_aw(&self, now_us: u64) -> u16 {
            let eaw_period_tu = u64::from(self.presence_mode) * u64::from(self.aw_period_tu);
            let time_since_tu = us_to_tu(now_us.saturating_sub(self.last_update_us));
            let current_aw = u64::from(self.aw_counter)
                + (time_since_tu % eaw_period_tu) / u64::from(self.aw_period_tu)
                + u64::from(self.presence_mode) * (time_since_tu / eaw_period_tu);
            current_aw as u16
        }

        pub fn current_eaw(&self, now_us: u64) -> u16 {
            self.current_aw(now_us) / self.presence_mode
        }
    }

    fn tu_to_us(tu: u64) -> u64 {
        tu * 1024
    }

    fn us_to_tu(us: u64) -> u64 {
        us / 1024
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TsftBridge {
        offset_us: Option<i64>,
        sanity_clamp_us: i64,
    }

    impl Default for TsftBridge {
        fn default() -> Self {
            Self {
                offset_us: None,
                sanity_clamp_us: 100_000,
            }
        }
    }

    impl TsftBridge {
        pub fn timestamp_rx(&mut self, hw_tsft_us: u64, host_now_us: u64) -> u64 {
            let instant_offset = host_now_us as i64 - hw_tsft_us as i64;
            let Some(offset) = self.offset_us else {
                self.offset_us = Some(instant_offset);
                return host_now_us;
            };

            let candidate = hw_tsft_us as i64 + offset;
            if (candidate - host_now_us as i64).abs() > self.sanity_clamp_us {
                self.offset_us = Some(instant_offset);
                return host_now_us;
            }

            let next_offset = offset + ((instant_offset - offset) >> 4);
            self.offset_us = Some(next_offset);
            (hw_tsft_us as i64 + next_offset) as u64
        }

        /// The last filtered TSF→monotonic offset (microseconds), or `None` if
        /// no frame has been timestamped yet. Exposed for the introspection
        /// `/status tsf_offset_us` metric (FILIN_SYNC_QUALITY.md Part A).
        pub fn offset_us(&self) -> Option<i64> {
            self.offset_us
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn updates_schedule_from_hardware_timestamped_master_sync() {
            let mut sync = SyncState::new(1_000_000);

            sync.update_from_master(2_000_000, 20, 18);

            assert_eq!(sync.last_update_us, 2_000_000 - (64 - 20) * 1024);
            assert_eq!(sync.aw_counter, 16);
        }

        #[test]
        fn bridges_tsft_to_monotonic_with_ema_offset() {
            let mut bridge = TsftBridge::default();

            assert_eq!(bridge.timestamp_rx(1_000, 101_000), 101_000);
            assert_eq!(bridge.timestamp_rx(2_000, 104_000), 102_125);
        }

        #[test]
        fn computes_zero_sync_error_for_matching_master_schedule() {
            let mut sync = SyncState::new(0);
            sync.update_from_master(2_000_000, 20, 18);

            assert_eq!(sync.sync_error_tu(2_000_000, 20, 18), 0);
        }

        #[test]
        fn update_from_master_does_not_underflow_on_small_rx_time() {
            // Regression (FILIN_AUDIT_FIXES.md #2): early after start, or
            // after a TsftBridge re-lock, rx_hw_time_us can be smaller than
            // tu_to_us(eaw_period - time_to_next_aw). The unchecked `-` would
            // wrap to a huge u64 and corrupt last_update_us. Saturating to 0
            // is safe (the next real frame corrects it).
            let mut sync = SyncState::new(0);
            // rx_hw_time_us=100, time_to_next_aw_tu=20 → subtract (64-20)*1024
            // = 45056us. 100 < 45056 → would underflow without saturating_sub.
            sync.update_from_master(100, 20, 18);
            assert_eq!(
                sync.last_update_us, 0,
                "must saturate to 0, not wrap to u64::MAX-..."
            );
            // Normal operation still works after recovery.
            sync.update_from_master(2_000_000, 20, 18);
            assert_eq!(sync.last_update_us, 2_000_000 - (64 - 20) * 1024);
        }

        #[test]
        fn next_aw_tu_does_not_underflow_when_now_before_last_update() {
            // If now_us < last_update_us (clock step / TSF re-lock), the old
            // code would wrap. Saturating keeps the function sane.
            let sync = SyncState::new(1_000_000); // last_update_us = 1_000_000
            let result = sync.next_aw_tu(500_000); // now < last_update
            // Should NOT panic or return garbage; the result is a valid u16.
            assert!(result > 0, "next_aw_tu must return a positive value");
        }

        #[test]
        fn current_aw_does_not_underflow_when_now_before_last_update() {
            let sync = SyncState::new(1_000_000);
            let result = sync.current_aw(500_000);
            // Should NOT panic or wrap to a huge value.
            assert!(result < 1000, "current_aw must stay reasonable");
        }
    }
}

pub mod channel {
    /// True if `channel` is a 5GHz DFS channel that is **No-IR** (No Initiate
    /// Radiation): a station may not transmit there until it has detected an
    /// existing master (radar-avoidance regulatory rule). The carl9170 honours
    /// this and silently drops injects on these channels, so filin must not
    /// follow a master anchored here — it could never TX (announce/data) and
    /// would be stranded off the AirDrop social channels (6/44/149).
    ///
    /// DFS set: UNII-2A (52,56,60,64) and UNII-2C (100–144 in steps of 4).
    /// Channel 0 means "unknown/no anchor" and is treated as not No-IR so an
    /// as-yet-undecoded peer is still eligible.
    pub fn is_no_ir_channel(channel: u8) -> bool {
        matches!(channel, 52 | 56 | 60 | 64 | 100..=144 if channel.is_multiple_of(4))
    }

    pub fn channel_to_frequency_mhz(channel: u16) -> Option<u16> {
        match channel {
            0 => None,
            14 => Some(2484),
            1..=13 => Some(2407 + channel * 5),
            32..=181 => Some(5000 + channel * 5),
            182..=196 => Some(4000 + channel * 5),
            _ => None,
        }
    }

    pub fn channel_for_eaw(sequence: &[u16], current_eaw: u16) -> Option<u16> {
        if sequence.is_empty() {
            return None;
        }
        sequence
            .get(usize::from(current_eaw) % sequence.len())
            .copied()
            .filter(|channel| *channel != 0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn no_ir_channels_are_the_dfs_set_only() {
            // DFS / No-IR: UNII-2A 52-64 and UNII-2C 100-144 (step 4).
            for ch in [52, 56, 60, 64, 100, 104, 120, 140, 144] {
                assert!(is_no_ir_channel(ch), "ch{ch} should be No-IR");
            }
            // TX-capable: social channels and the UNII-1/UNII-3 non-DFS set,
            // plus 0 (unknown anchor) and non-grid numbers.
            for ch in [0, 6, 36, 40, 44, 48, 149, 153, 157, 161, 165, 96, 148] {
                assert!(!is_no_ir_channel(ch), "ch{ch} should be TX-capable");
            }
        }

        #[test]
        fn converts_common_awdl_channels_to_frequency() {
            assert_eq!(channel_to_frequency_mhz(6), Some(2437));
            assert_eq!(channel_to_frequency_mhz(44), Some(5220));
            assert_eq!(channel_to_frequency_mhz(149), Some(5745));
        }

        #[test]
        fn selects_channel_by_eaw_slot() {
            let sequence = [44, 44, 149, 6, 0, 0, 44, 149, 6, 44, 0, 0, 149, 44, 6, 44];

            assert_eq!(channel_for_eaw(&sequence, 0), Some(44));
            assert_eq!(channel_for_eaw(&sequence, 2), Some(149));
            assert_eq!(channel_for_eaw(&sequence, 17), Some(44));
        }
    }
}

pub mod schedule {
    //! Faithful port of owl `schedule.c` / `schedule.h`: availability-window
    //! gating for action/data transmission.
    use crate::sync::SyncState;

    pub const CHANSEQ_LEN: usize = 16;
    /// owl `AWDL_MULTICAST_GUARD_TU` (owl/src/schedule.h:29).
    pub const MULTICAST_GUARD_TU: u16 = 16;
    /// owl `AWDL_UNICAST_GUARD_TU` (owl/src/schedule.h:28).
    pub const UNICAST_GUARD_TU: u16 = 3;

    const TU_US: u64 = 1024;

    /// owl `awdl_is_multicast_eaw` (owl/src/schedule.c:43-46): slots 0 and 10
    /// are the AWDL multicast availability windows.
    pub fn is_multicast_eaw(sync: &SyncState, now_us: u64) -> bool {
        let slot = usize::from(sync.current_eaw(now_us) % CHANSEQ_LEN as u16);
        slot == 0 || slot == 10
    }

    /// Channel number (opclass encoding) for a sequence slot, or 0 if null.
    pub fn slot_channel(channel_sequence: &[[u8; 2]; CHANSEQ_LEN], slot: usize) -> u8 {
        channel_sequence[slot % CHANSEQ_LEN][0]
    }

    /// owl `awdl_same_channel_as_peer` (owl/src/schedule.c:30-41): true if we
    /// and the peer are on the same non-zero channel at the current EAW slot.
    /// When filin has adopted the master's sequence, this is always true for
    /// the master; it may differ for non-master peers.
    pub fn same_channel_as_peer(
        own_seq: &[[u8; 2]; CHANSEQ_LEN],
        peer_seq: &[[u8; 2]; CHANSEQ_LEN],
        sync: &SyncState,
        now_us: u64,
    ) -> bool {
        let own_slot = usize::from(sync.current_eaw(now_us) % CHANSEQ_LEN as u16);
        let own_chan = slot_channel(own_seq, own_slot);
        let peer_chan = slot_channel(peer_seq, own_slot);
        own_chan != 0 && own_chan == peer_chan
    }

    /// owl `awdl_can_send_in` (owl/src/schedule.c:48-55): returns 0 when it is
    /// safe to send now (inside the guard-free core of the AW), a positive
    /// wait meaning "we are in the leading edge of the next AW / trailing guard,
    /// wait this long", or a negative wait meaning "we are inside the guard,
    /// hold this long before retrying".
    ///
    /// owl returns the wait as fractional `double` *seconds*; filin returns it
    /// as i64 *microseconds*. The sign and the zero/non-zero distinction are
    /// preserved (the only properties callers depend on). Returning seconds via
    /// integer division would floor every guard-sized quantity (≤ 16 TU ≈
    /// 16_384 us) to 0, silently disabling the guard gate entirely.
    pub fn can_send_in(sync: &SyncState, now_us: u64, guard_tu: u16) -> i64 {
        let next_aw = sync.next_aw_us(now_us);
        let guard = tu_to_us(u64::from(guard_tu));
        let eaw = tu_to_us(64); // presence_mode(4) * aw_period(16) TU
        if next_aw < guard {
            -((guard - next_aw) as i64)
        } else if eaw - next_aw < guard {
            (guard - (eaw - next_aw)) as i64
        } else {
            0
        }
    }

    fn tu_to_us(tu: u64) -> u64 {
        tu * TU_US
    }

    /// Decision returned by [`data_send_decision`]: whether the TX path
    /// should inject a data frame now (`Send`) or hold it for retry (`Hold`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DataSendDecision {
        /// Inject the frame now on the current channel/availability window.
        Send,
        /// Hold the frame and retry on the next availability window. The
        /// condition is transient (wrong slot / inside guard / peer not yet
        /// co-channel) and expected to clear within an AW cycle.
        Hold,
        /// Discard the frame — it can never become sendable. owl's
        /// `awdl_send_unicast` frees (drops) a unicast frame addressed to a
        /// non-peer; holding it instead would block the host TAP indefinitely
        /// (the retry loop stops reading new host frames while one is pending).
        Drop,
    }

    /// Pure TX-gate decision that ports owl's split between
    /// `awdl_send_unicast` (owl/daemon/core.c:202-244, per-peer and
    /// channel-gated) and `awdl_send_multicast` (owl/daemon/core.c:246-277,
    /// sent on the current AW channel with no per-peer gate).
    ///
    /// Given a data frame's destination MAC, the current channel, our own
    /// channel sequence, the peer table, and the current sync/now, decide
    /// whether the runtime TX path should inject the frame now.
    ///
    /// - **Multicast/broadcast** destinations — the Ethernet I/G bit is set,
    ///   `dst[0] & 0x01 == 1`, which covers IPv6 multicast `33:33:..` (mDNS
    ///   announce `ff02::fb`, NDP solicitations `33:33:ff:..`) and broadcast
    ///   `ff:ff:..` — are NOT peer-gated. Like owl's `awdl_send_multicast`,
    ///   they inject on the current channel during the multicast availability
    ///   window (slots 0 and 10) outside the multicast guard interval,
    ///   regardless of whether any single peer happens to be on this channel.
    ///   A multicast frame has no single destination peer, so the
    ///   "same channel as peer" lookup must not apply.
    /// - **Unicast** destinations keep owl's `awdl_send_unicast` gate: sent
    ///   only when the destination peer is known and on the same channel this
    ///   EAW, outside the unicast guard interval.
    pub fn data_send_decision(
        dst: &[u8; 6],
        current_channel: u8,
        own_seq: &[[u8; 2]; CHANSEQ_LEN],
        peers: &crate::peers::PeerTable,
        sync: &SyncState,
        now_us: u64,
    ) -> DataSendDecision {
        if current_channel == 0 {
            return DataSendDecision::Hold; // null slot: not available
        }
        if dst[0] & 0x01 == 1 {
            // owl awdl_send_multicast (core.c:254-255): multicast EAW +
            // multicast guard, never the per-peer same-channel gate.
            if !is_multicast_eaw(sync, now_us) {
                return DataSendDecision::Hold;
            }
            if can_send_in(sync, now_us, MULTICAST_GUARD_TU) != 0 {
                return DataSendDecision::Hold;
            }
            DataSendDecision::Send
        } else {
            // owl awdl_send_unicast (core.c:222-223): per-peer gate.
            let Some(peer) = peers.get(dst) else {
                // owl awdl_send_unicast frees a non-peer frame. Drop, don't
                // hold — a held non-peer frame would block the TAP forever.
                return DataSendDecision::Drop;
            };
            if !same_channel_as_peer(own_seq, &peer.sequence, sync, now_us) {
                return DataSendDecision::Hold; // peer is on a different channel
            }
            if can_send_in(sync, now_us, UNICAST_GUARD_TU) != 0 {
                return DataSendDecision::Hold; // inside unicast guard interval
            }
            DataSendDecision::Send
        }
    }

    /// Maximum number of pending multicast frames buffered in the spread
    /// queue. Bounds memory if the host TAP feeds multicast faster than one
    /// hop cycle can drain it.
    pub const MULTICAST_SPREAD_CAP: usize = 32;

    /// One full hop cycle in microseconds: 16 EAW slots * 64 TU per EAW
    /// (presence_mode 4 * aw_period 16 TU) * 1024 us/TU. Pending multicast
    /// frames older than this are aged out so a frame whose remaining
    /// channels never recur (e.g. the master sequence changed mid-cycle)
    /// cannot leak.
    pub const HOP_CYCLE_US: u64 = 16 * 64 * TU_US;

    /// The distinct non-zero channel numbers in a channel sequence, deduped
    /// and sorted ascending. These are the channels a multicast/broadcast
    /// frame must be transmitted on so that a peer listening on ANY slot of
    /// the adopted master sequence hears it during one hop cycle. If the
    /// sequence contains no non-zero channels (filin has not adopted a
    /// master, or all slots are null), falls back to `[anchor]` — i.e. the
    /// pre-spread single-channel behaviour.
    pub fn distinct_channels(chanseq: &[[u8; 2]; CHANSEQ_LEN], anchor: u8) -> Vec<u8> {
        use std::collections::BTreeSet;
        let mut set: BTreeSet<u8> = BTreeSet::new();
        for slot in chanseq.iter() {
            if slot[0] != 0 {
                set.insert(slot[0]);
            }
        }
        if set.is_empty() {
            if anchor != 0 {
                vec![anchor]
            } else {
                Vec::new()
            }
        } else {
            set.into_iter().collect()
        }
    }

    /// AWDL social channels (owl `CHAN_OPCLASS_*`, owl/src/channel.h:51-53)
    /// that Apple devices scan on for AirDrop discovery. filin must visit
    /// ALL of these during its hop cycle — if the committed master only
    /// advertises a subset (e.g. a master in an active data session may use
    /// only 44 with some 6), an iPhone scanning on 149 will never discover
    /// filin.
    pub const AWDL_SOCIAL_CHANNELS: [(u8, u8); 3] = [
        (6, 0x51),    // CHAN_OPCLASS_6
        (44, 0x80),   // CHAN_OPCLASS_44
        (149, 0x80),  // CHAN_OPCLASS_149
    ];

    /// The distinct social-channel NUMBERS (6, 44, 149), sorted ascending.
    pub const AWDL_SOCIAL_CHANNEL_NUMS: [u8; 3] = [6, 44, 149];

    /// Extract the adopted master's **anchor channel** — the slot-0 / primary
    /// channel of the decoded master sequence — so filin can re-broadcast and
    /// dwell on it adaptively (FILIN_ANCHOR_REBROADCAST.md). Returns slot 0's
    /// channel if non-zero; otherwise the most-frequent non-zero channel
    /// (the "dominant" one); or 0 if the sequence is all-null (no master).
    ///
    /// On a frequency tie the lowest channel number wins (deterministic).
    pub fn master_anchor_channel(chanseq: &[[u8; 2]; CHANSEQ_LEN]) -> u8 {
        if chanseq[0][0] != 0 {
            return chanseq[0][0];
        }
        // Slot 0 is null: fall back to the most-frequent non-zero channel.
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
        for slot in chanseq.iter() {
            if slot[0] != 0 {
                *counts.entry(slot[0]).or_default() += 1;
            }
        }
        // BTreeMap iterates ascending by channel. On a tie, prefer the lowest
        // channel: pick the first whose count equals the max.
        let max_count = counts.values().copied().max().unwrap_or(0);
        counts
            .into_iter()
            .find(|(_, c)| *c == max_count)
            .map(|(ch, _)| ch)
            .unwrap_or(0)
    }

    /// The set of channels filin should re-broadcast the cached mDNS announce
    /// on: always the three AWDL social channels (6/44/149), plus the adopted
    /// master's anchor if it is not already among them (e.g. 52 on a
    /// DFS-anchored cluster). Sorted ascending, deduped. If `anchor` is 0
    /// (no master) only the social channels are returned.
    pub fn rebroadcast_channels(anchor: u8) -> Vec<u8> {
        use std::collections::BTreeSet;
        let mut set: BTreeSet<u8> = AWDL_SOCIAL_CHANNEL_NUMS.iter().copied().collect();
        if anchor != 0 {
            set.insert(anchor);
        }
        set.into_iter().collect()
    }

    /// Rewrite every No-IR/DFS channel slot in `seq` to a TX-capable social
    /// channel (6/44/149, round-robin by occurrence). filin may adopt a master
    /// that hops/anchors on No-IR channels (for AW *timing*), but the carl9170
    /// can neither transmit nor usefully receive there, so the monitor must
    /// never be tuned to them — we substitute a social channel so our announce
    /// still goes out in-window. Null slots (0) are left untouched for
    /// [`ensure_social_coverage`]. Returns the No-IR channels that were
    /// replaced (in slot order), empty if there were none.
    pub fn replace_no_ir_slots(seq: &mut [[u8; 2]; CHANSEQ_LEN]) -> Vec<u8> {
        let mut replaced = Vec::new();
        for slot in seq.iter_mut() {
            if slot[0] != 0 && crate::channel::is_no_ir_channel(slot[0]) {
                let (ch, opclass) = AWDL_SOCIAL_CHANNELS[replaced.len() % AWDL_SOCIAL_CHANNELS.len()];
                replaced.push(slot[0]);
                *slot = [ch, opclass];
            }
        }
        replaced
    }

    /// Ensure every AWDL social channel (6, 44, 149) appears at least once
    /// in the channel sequence by injecting any missing social channels into
    /// null or duplicate slots. This guarantees filin physically visits 149
    /// (where iPhones scan for AirDrop) even when the adopted master's
    /// sequence omits it.
    ///
    /// Slot selection: prefer null slots (channel 0) so we don't remove any
    /// channel the master advertised. If there are no nulls, overwrite a
    /// slot whose channel appears in other slots (a duplicate), preserving
    /// at least one occurrence of every distinct channel. Returns the
    /// channels that were injected (empty if the sequence already covered
    /// all social channels).
    ///
    /// After guaranteeing one slot per social channel, any REMAINING null
    /// slots are filled biased toward `anchor` (FILIN_ANCHOR_REBROADCAST.md):
    /// the anchor alternates with round-robin social channels so the master's
    /// primary channel gets materially more dwell than the bare ~6% it had
    /// from the master's sequence alone. If `anchor` is 0 (no master), falls
    /// back to the original `[149, 6, 44]` round-robin starting at 149.
    pub fn ensure_social_coverage(seq: &mut [[u8; 2]; CHANSEQ_LEN], anchor: u8) -> Vec<u8> {
        let mut injected = Vec::new();
        for &(ch, opclass) in &AWDL_SOCIAL_CHANNELS {
            if seq.iter().any(|s| s[0] == ch) {
                continue; // already present
            }
            if let Some(i) = find_injectable_slot(seq) {
                seq[i] = [ch, opclass];
                injected.push(ch);
            }
        }
        // Fill remaining null slots: biased toward the anchor (if any), else
        // the original 149-first round-robin.
        fill_nulls_biased(seq, anchor);
        injected
    }

    /// Fill remaining null (channel-0) slots. When `anchor` is non-zero, the
    /// anchor alternates with round-robin social channels (`[anchor, 149,
    /// anchor, 6, anchor, 44, ...]`) so the master's primary channel gets
    /// ~50% of null fills — materially more than the ~6% it would get from a
    /// flat round-robin. When `anchor` is 0, the original `[149, 6, 44]`
    /// round-robin starting at 149 is used (backward-compatible).
    fn fill_nulls_biased(seq: &mut [[u8; 2]; CHANSEQ_LEN], anchor: u8) {
        if anchor == 0 {
            // Original behaviour: round-robin 149/6/44 starting at 149.
            let order = [(149u8, 0x80u8), (6, 0x51), (44, 0x80)];
            let mut idx = 0;
            for slot in seq.iter_mut() {
                if slot[0] == 0 {
                    let (ch, opclass) = order[idx % order.len()];
                    *slot = [ch, opclass];
                    idx += 1;
                }
            }
            return;
        }
        // Anchor-biased: alternate anchor with social channels.
        let anchor_opclass = opclass_for_channel(anchor);
        let socials = [(149u8, 0x80u8), (6, 0x51), (44, 0x80)];
        let mut social_idx = 0;
        let mut anchor_next = true;
        for slot in seq.iter_mut() {
            if slot[0] != 0 {
                continue;
            }
            if anchor_next {
                *slot = [anchor, anchor_opclass];
            } else {
                let (ch, opclass) = socials[social_idx % socials.len()];
                *slot = [ch, opclass];
                social_idx += 1;
            }
            anchor_next = !anchor_next;
        }
    }

    /// opclass byte for any channel (falls back to 0x80 like owl's default).
    fn opclass_for_channel(channel: u8) -> u8 {
        match channel {
            6 => 0x51,
            44 | 149 => 0x80,
            _ => 0x80,
        }
    }

    /// Find a slot suitable for social-channel injection: a null slot, or
    /// failing that, a slot whose channel appears in at least one other slot
    /// (a duplicate we can safely overwrite).
    fn find_injectable_slot(seq: &[[u8; 2]; CHANSEQ_LEN]) -> Option<usize> {
        // Prefer a null slot.
        if let Some(i) = seq.iter().position(|s| s[0] == 0) {
            return Some(i);
        }
        // Else find a duplicate slot (channel appears more than once).
        for i in 0..CHANSEQ_LEN {
            let ch = seq[i][0];
            if seq.iter().filter(|s| s[0] == ch).count() > 1 {
                return Some(i);
            }
        }
        None
    }

    /// A multicast/broadcast frame pending transmission on the remaining
    /// channels of the master hop cycle.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PendingMulticast {
        /// Radiotap-prefixed IEEE 802.11 data frame bytes to inject.
        pub frame: Vec<u8>,
        /// Distinct channels this frame has NOT yet been transmitted on.
        pub channels_todo: std::collections::BTreeSet<u8>,
        /// Host time when enqueued, for TTL age-out.
        pub enqueued_at_us: u64,
    }

    /// Bounded FIFO queue of multicast frames being spread across the hop
    /// cycle.
    ///
    /// Ports owl's intent from `awdl_send_multicast` (owl/daemon/core.c:
    /// 246-277) but extends it: instead of emitting a multicast frame once
    /// on the current channel, each enqueued frame is transmitted on every
    /// distinct channel of the adopted master channel sequence over the next
    /// hop cycle, so a peer listening on any of those channels (e.g.
    /// 6/44/149) hears it.
    ///
    /// The queue is bounded (`cap`); enqueuing past the cap evicts the oldest
    /// entry so a host-TAP flood cannot grow it unboundedly. Entries older
    /// than `ttl_us` (one hop cycle) are aged out during [`Self::drain_for_channel`]
    /// so a frame whose remaining channels never recur does not leak.
    #[derive(Debug)]
    pub struct MulticastSpread {
        entries: std::collections::VecDeque<PendingMulticast>,
        cap: usize,
        ttl_us: u64,
    }

    impl MulticastSpread {
        pub fn new(cap: usize, ttl_us: u64) -> Self {
            Self {
                entries: std::collections::VecDeque::new(),
                cap,
                ttl_us,
            }
        }

        pub fn len(&self) -> usize {
            self.entries.len()
        }

        pub fn is_empty(&self) -> bool {
            self.entries.is_empty()
        }

        /// Enqueue a frame to be spread across the distinct channels of
        /// `chanseq` (falling back to `[anchor]` per [`distinct_channels`]).
        /// Returns `true` iff the frame was enqueued (there was at least one
        /// target channel). If the queue is full, the oldest entry is evicted
        /// first so the most recent announce always gets its hop cycle.
        pub fn enqueue(
            &mut self,
            frame: Vec<u8>,
            chanseq: &[[u8; 2]; CHANSEQ_LEN],
            anchor: u8,
            now_us: u64,
        ) -> bool {
            let channels: std::collections::BTreeSet<u8> =
                distinct_channels(chanseq, anchor).into_iter().collect();
            if channels.is_empty() {
                return false;
            }
            while self.entries.len() >= self.cap {
                self.entries.pop_front();
            }
            self.entries.push_back(PendingMulticast {
                frame,
                channels_todo: channels,
                enqueued_at_us: now_us,
            });
            true
        }

        /// Drain frames that should be transmitted on `channel` now. For each
        /// pending frame whose remaining-channel set contains `channel`,
        /// returns its frame bytes and removes `channel` from its todo set;
        /// frames whose todo set is then empty are dropped (fully spread).
        /// Entries older than `ttl_us` are dropped without being returned
        /// (aged out). Returns frame bytes to inject, in FIFO order.
        pub fn drain_for_channel(&mut self, channel: u8, now_us: u64) -> Vec<Vec<u8>> {
            let ttl = self.ttl_us;
            let mut out: Vec<Vec<u8>> = Vec::new();
            let mut keep = std::collections::VecDeque::with_capacity(self.entries.len());
            while let Some(mut entry) = self.entries.pop_front() {
                if now_us.saturating_sub(entry.enqueued_at_us) > ttl {
                    continue; // aged out: drop without sending
                }
                if entry.channels_todo.remove(&channel) {
                    // Clone: the entry stays queued for its remaining channels,
                    // so its frame bytes must survive until the todo set empties.
                    out.push(entry.frame.clone());
                    if !entry.channels_todo.is_empty() {
                        keep.push_back(entry);
                    }
                } else {
                    keep.push_back(entry);
                }
            }
            self.entries = keep;
            out
        }
    }

    // --- Part A: cached mDNS announce re-broadcast -------------------------

    /// Minimum dwell between re-broadcasts of the cached announce on the
    /// SAME social channel (~1 s). Prevents flooding while ensuring every
    /// ~1 s of dwell on 149 the iPhone gets a fresh announce.
    pub const ANNOUNCE_REBROADCAST_MIN_GAP_US: u64 = 1_000_000;

    /// How long the cached announce stays valid after the last refresh from
    /// the TAP. A few × the luftlift re-announce interval (~10 s): if no new
    /// multicast arrives for 45 s, stop advertising a (probably dead)
    /// receiver.
    pub const ANNOUNCE_CACHE_TTL_US: u64 = 45_000_000;

    /// Cache of the latest multicast mDNS announce read from the awdl0 TAP,
    /// re-broadcast on every social-channel visit so discovery does not
    /// depend on luftlift's ~10 s cadence aligning with a rare 149 hop.
    ///
    /// At most one frame is cached at a time (the latest); it is refreshed
    /// whenever a new multicast frame arrives and expires if no refresh
    /// arrives for [`ANNOUNCE_CACHE_TTL_US`]. Re-broadcasts are rate-limited
    /// per-channel to [`ANNOUNCE_REBROADCAST_MIN_GAP_US`].
    #[derive(Debug)]
    pub struct AnnounceCache {
        frame: Option<Vec<u8>>,
        cached_at_us: u64,
        /// Host time of the last re-broadcast per channel, for rate limiting.
        last_sent: std::collections::BTreeMap<u8, u64>,
        min_gap_us: u64,
        ttl_us: u64,
    }

    impl AnnounceCache {
        pub fn new(min_gap_us: u64, ttl_us: u64) -> Self {
            Self {
                frame: None,
                cached_at_us: 0,
                last_sent: std::collections::BTreeMap::new(),
                min_gap_us,
                ttl_us,
            }
        }

        /// Refresh the cache with a new multicast frame from the TAP.
        /// Always keeps just the latest frame (bounded).
        pub fn refresh(&mut self, frame: Vec<u8>, now_us: u64) {
            self.frame = Some(frame);
            self.cached_at_us = now_us;
        }

        /// Returns the cached frame to re-broadcast on `channel` now, iff:
        /// the cache is live (not expired), the channel is in `allowed`, and
        /// we have not already re-broadcast on this channel within the
        /// rate-limit window. Updates the per-channel rate-limit clock on a
        /// hit. Returns `None` if suppressed/expired/empty.
        ///
        /// `allowed` is the set of channels worth re-broadcasting on —
        /// typically [`rebroadcast_channels`] (the AWDL social channels plus
        /// the adopted master's anchor). This makes filin adaptive: when the
        /// master anchors on e.g. 52, the cached announce is re-broadcast on
        /// 52 too, not just 6/44/149.
        pub fn rebroadcast(
            &mut self,
            channel: u8,
            now_us: u64,
            allowed: &[u8],
        ) -> Option<&[u8]> {
            // Expire stale cache so a dead receiver stops being advertised.
            // Done before borrowing self.frame below to avoid a borrow conflict.
            if self.frame.is_some()
                && now_us.saturating_sub(self.cached_at_us) > self.ttl_us
            {
                self.frame = None;
            }
            let frame = self.frame.as_deref()?;
            if !allowed.contains(&channel) {
                return None; // not a re-broadcast channel
            }
            if let Some(&last) = self.last_sent.get(&channel) {
                if now_us.saturating_sub(last) < self.min_gap_us {
                    return None; // rate-limited on this channel
                }
            }
            self.last_sent.insert(channel, now_us);
            Some(frame)
        }

        /// Whether the cache currently holds a non-expired frame.
        pub fn is_live(&self, now_us: u64) -> bool {
            self.frame
                .is_some()
                && now_us.saturating_sub(self.cached_at_us) <= self.ttl_us
        }
    }

    // --- Bug C: sticky transfer channel -------------------------------------

    /// Idle window after the last received unicast data frame from a peer
    /// during which filin keeps the radio PINNED to that peer's channel and
    /// suppresses channel hopping + master re-adoption.
    ///
    /// This must cover not just gaps between TCP segments of a bulk /Upload,
    /// but the **user-interaction gap between AirDrop's `/Discover` (browsing,
    /// fills the picker) and `/Ask` (fires when the user taps send)** — which
    /// is seconds, not milliseconds. The original ~131 ms (2 EAW) released the
    /// pin long before the `/Ask` arrived, so filin hopped away and the `/Ask`
    /// connection never reached the host (observed live: `/Discover` lands,
    /// `/Ask` never does, sender shows "declined"). 6 s holds the peer's
    /// channel across a normal tap while still releasing promptly once idle.
    pub const TRANSFER_IDLE_TIMEOUT_US: u64 = 6_000_000;

    /// Record of the most recent unicast data RX used to pin the radio to
    /// the active-transfer peer's channel (Bug C).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TransferPin {
        pub peer: [u8; 6],
        pub channel: u8,
        pub last_rx_us: u64,
    }

    /// Pure decision for the sticky-transfer pin (Bug C): returns the
    /// `(peer, channel)` filin must hold while a unicast data flow is
    /// active, or `None` once the idle timeout has elapsed (transfer
    /// complete / peer went quiet). A pin whose recorded channel is 0 is
    /// treated as inactive. Used both to override [`desired_channel`] and to
    /// suppress master re-adoption ([`master_adoption_decision`]).
    pub fn transfer_active(
        pin: Option<&TransferPin>,
        now_us: u64,
        idle_timeout_us: u64,
    ) -> Option<([u8; 6], u8)> {
        let pin = pin?;
        if pin.channel == 0 {
            return None;
        }
        if now_us.saturating_sub(pin.last_rx_us) > idle_timeout_us {
            return None;
        }
        Some((pin.peer, pin.channel))
    }

    // --- Bug B: master-adoption hysteresis ----------------------------------

    /// How long a challenger must persistently win the election before filin
    /// commits to adopting its channel sequence. Stops the master thrash
    /// seen on a contended link (re-adopting ~44×/min flip-flopping between
    /// Apple devices). Two EAWs mirrors the transfer pin window so a single
    /// stray challenger frame cannot deschedule an in-flight transfer.
    pub const MASTER_DEBOUNCE_US: u64 = 2 * 64 * TU_US;

    /// Result of [`master_adoption_decision`]: which master to commit to now.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MasterAdoption {
        /// Keep the currently-committed master (challenger has not yet won
        /// the debounce window).
        Keep,
        /// Commit to the new election winner.
        Adopt,
    }

    /// Pure master-adoption hysteresis decision (Bug B). Given the
    /// currently-committed master, the latest election `winner`, the
    /// pending challenger and how long it has been winning, returns whether
    /// to commit to the winner now (`Adopt`) or hold the current master
    /// (`Keep`).
    ///
    /// - If the winner is already the committed master, returns `Adopt`
    ///   (no-op refresh; clears the pending challenger).
    /// - If the winner is the pending challenger AND it has been winning
    ///   for at least `debounce_us`, returns `Adopt`.
    /// - Otherwise returns `Keep` — the challenger must persist.
    pub fn master_adoption_decision(
        committed_master: [u8; 6],
        winner: [u8; 6],
        pending_master: [u8; 6],
        pending_since_us: u64,
        now_us: u64,
        debounce_us: u64,
    ) -> MasterAdoption {
        if winner == committed_master {
            return MasterAdoption::Adopt;
        }
        if winner == pending_master
            && now_us.saturating_sub(pending_since_us) >= debounce_us
        {
            return MasterAdoption::Adopt;
        }
        MasterAdoption::Keep
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn synced() -> SyncState {
            SyncState::new(1_000_000)
        }

        #[test]
        fn mid_eaw_is_sendable_outside_guard() {
            // last_update at 1_000_000; at 1_032_000 we are ~32 TU into the EAW
            // (well clear of the 16 TU guard on both sides).
            let sync = synced();
            assert_eq!(can_send_in(&sync, 1_032_768, MULTICAST_GUARD_TU), 0);
        }

        #[test]
        fn leading_guard_returns_negative() {
            // last_update at 1_000_000; at 1_050_000 we are 50_000 us into the
            // 65_536 us EAW, so next_aw = 15_536 us < the 16 TU (16_384 us)
            // multicast guard. owl returns a negative wait; the gate must be
            // non-zero so the frame is held, not sent through the guard.
            let sync = synced();
            assert!(can_send_in(&sync, 1_050_000, MULTICAST_GUARD_TU) < 0);
        }

        #[test]
        fn trailing_guard_returns_positive() {
            // At the AW boundary (1_000_000) we sit in the trailing guard:
            // eaw - next_aw = 0 < guard, so owl returns a positive wait.
            let sync = synced();
            assert!(can_send_in(&sync, 1_000_000, MULTICAST_GUARD_TU) > 0);
        }

        #[test]
        fn detects_multicast_eaw_slots() {
            let sync = synced();
            // current_eaw advances with time; verify slot 0 at the start.
            assert!(is_multicast_eaw(&sync, 1_000_000));
        }

        #[test]
        fn slot_channel_reads_chan_num_byte() {
            let seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert_eq!(slot_channel(&seq, 0), 44);
            assert_eq!(slot_channel(&seq, 15), 44);
        }

        #[test]
        fn same_channel_when_sequences_match() {
            let sync = synced();
            let seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert!(same_channel_as_peer(&seq, &seq, &sync, 1_000_000));
        }

        #[test]
        fn different_channel_when_sequences_diverge() {
            let sync = synced();
            let own = [[44u8, 0x80u8]; CHANSEQ_LEN];
            let peer = {
                let mut s = [[149u8, 0x80u8]; CHANSEQ_LEN];
                s[0] = [0, 0]; // slot 0 is null for peer
                s
            };
            // slot 0: own is 44, peer is 0 (null) → not same
            assert!(!same_channel_as_peer(&own, &peer, &sync, 1_000_000));
        }

        // --- data_send_decision: the multicast/broadcast TX fix ---

        fn empty_peers() -> crate::peers::PeerTable {
            crate::peers::PeerTable::new()
        }

        fn peer_on_channel(addr: [u8; 6], chan: u8) -> crate::peers::PeerTable {
            // Build a peer table whose single peer advertises `chan` on every
            // slot of its channel sequence (so same_channel_as_peer is true
            // iff our own sequence also has `chan` on the current slot).
            let mut table = crate::peers::PeerTable::new();
            table.touch(addr, 1_000_000);
            table.get_mut(&addr).unwrap().sequence = [[chan, 0x80]; CHANSEQ_LEN];
            table
        }

        /// The mDNS announce multicast destination (ff02::fb maps to the
        /// Ethernet multicast address 33:33:00:00:00:fb). Its first octet has
        /// the I/G bit set.
        const MDNS_V6_MCAST: [u8; 6] = [0x33, 0x33, 0x00, 0x00, 0x00, 0xfb];

        #[test]
        fn multicast_dst_is_not_peer_gated() {
            // Regression: an mDNS announce (multicast) must be injected on
            // the current channel during a multicast EAW even when the peer
            // table is empty (no single destination peer to channel-match).
            // owl `awdl_send_multicast` (core.c:246-277).
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            // 1_032_768 is ~32 TU into slot 0 (multicast EAW), clear of the
            // 16 TU multicast guard on both sides.
            assert_eq!(
                data_send_decision(
                    &MDNS_V6_MCAST,
                    44,
                    &own_seq,
                    &empty_peers(),
                    &sync,
                    1_032_768,
                ),
                DataSendDecision::Send,
            );
        }

        #[test]
        fn broadcast_dst_is_not_peer_gated() {
            // ff:ff:ff:ff:ff:ff also has the I/G bit set and must not be
            // held waiting for a per-peer channel match.
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert_eq!(
                data_send_decision(
                    &[0xff; 6],
                    44,
                    &own_seq,
                    &empty_peers(),
                    &sync,
                    1_032_768,
                ),
                DataSendDecision::Send,
            );
        }

        #[test]
        fn multicast_dst_held_off_multicast_eaw() {
            // Multicast frames are sent during multicast EAWs (slots 0/10)
            // like owl; mid-EAW slot 1 is not a multicast EAW, so hold.
            // 1_065_536 is the start of EAW slot 1 (one full EAW after
            // 1_000_000).
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert_eq!(
                data_send_decision(
                    &MDNS_V6_MCAST,
                    44,
                    &own_seq,
                    &empty_peers(),
                    &sync,
                    1_098_304,
                ),
                DataSendDecision::Hold,
            );
        }

        #[test]
        fn multicast_dst_held_on_null_channel() {
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert_eq!(
                data_send_decision(
                    &MDNS_V6_MCAST,
                    0,
                    &own_seq,
                    &empty_peers(),
                    &sync,
                    1_032_768,
                ),
                DataSendDecision::Hold,
            );
        }

        #[test]
        fn unicast_dst_with_no_peer_is_dropped() {
            // owl `awdl_send_unicast` frees (drops) unicast to a non-peer.
            // It must NOT be held: a held frame stops the runtime from
            // reading further host frames, stalling the TAP permanently.
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            assert_eq!(
                data_send_decision(
                    &[0x02, 0, 0, 0, 0, 0x07],
                    44,
                    &own_seq,
                    &empty_peers(),
                    &sync,
                    1_032_768,
                ),
                DataSendDecision::Drop,
            );
        }

        #[test]
        fn unicast_dst_same_channel_as_peer_is_sent() {
            // Unicast keeps the per-peer same-channel gate: when the peer is
            // known and on our channel, send.
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            let peer_addr = [0x02, 0, 0, 0, 0, 0x07];
            let peers = peer_on_channel(peer_addr, 44);
            assert_eq!(
                data_send_decision(
                    &peer_addr,
                    44,
                    &own_seq,
                    &peers,
                    &sync,
                    1_032_768,
                ),
                DataSendDecision::Send,
            );
        }

        #[test]
        fn unicast_dst_different_channel_is_held() {
            // The unicast same-channel gate is preserved: peer on a
            // different channel this EAW → hold (frame buffered for retry).
            let sync = synced();
            let own_seq = [[44u8, 0x80u8]; CHANSEQ_LEN];
            let peer_addr = [0x02, 0, 0, 0, 0, 0x07];
            // Peer sequence has a null slot 0, so it is not on our channel
            // (44) during slot 0.
            let mut peers = crate::peers::PeerTable::new();
            peers.touch(peer_addr, 1_000_000);
            {
                let seq = &mut peers.get_mut(&peer_addr).unwrap().sequence;
                seq[0] = [0, 0];
            }
            assert_eq!(
                data_send_decision(&peer_addr, 44, &own_seq, &peers, &sync, 1_032_768),
                DataSendDecision::Hold,
            );
        }

        // --- MulticastSpread: spread mDNS announce across the hop cycle ---

        /// Build a 16-slot channel sequence from a slice of (chan, opclass)
        /// pairs, repeating the last pair to fill the remaining slots (so a
        /// short master chanseq like [6,44,149] fills the 16-slot array).
        fn chanseq(pairs: &[(u8, u8)]) -> [[u8; 2]; CHANSEQ_LEN] {
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            let last = *pairs.last().unwrap_or(&(0, 0));
            for (i, p) in pairs.iter().enumerate() {
                seq[i] = [p.0, 0];
            }
            for slot in seq.iter_mut().take(CHANSEQ_LEN).skip(pairs.len()) {
                slot[0] = last.0;
            }
            seq
        }

        #[test]
        fn distinct_channels_dedupes_master_sequence() {
            // Master chanseq hops 6 / 44 / 149 (and repeats). Distinct set is
            // [6, 44, 149] — the channels a multicast announce must cover.
            let seq = chanseq(&[(6, 0), (44, 0), (149, 0)]);
            assert_eq!(distinct_channels(&seq, 44), vec![6, 44, 149]);
        }

        #[test]
        fn distinct_channels_ignores_null_slots() {
            // Null (0) slots are skipped, not treated as channel 0.
            let seq = chanseq(&[(6, 0), (0, 0), (149, 0)]);
            assert_eq!(distinct_channels(&seq, 44), vec![6, 149]);
        }

        #[test]
        fn distinct_channels_falls_back_to_anchor_when_no_master() {
            // No master adopted: all-zero sequence → single anchor channel,
            // i.e. the pre-spread single-channel behaviour.
            let seq = [[0u8, 0u8]; CHANSEQ_LEN];
            assert_eq!(distinct_channels(&seq, 44), vec![44]);
        }

        // --- master anchor extraction (FILIN_ANCHOR_REBROADCAST.md) ---

        #[test]
        fn master_anchor_extracts_slot_zero_channel() {
            // The live iPhone cluster's decoded sequence starts with 52 at
            // slot 0: [52,0,44,0,0,0,0,0,6,0,44,0,...]. The anchor must be 52.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [52, 0x80];
            seq[2] = [44, 0x80];
            seq[8] = [6, 0x51];
            seq[10] = [44, 0x80];
            assert_eq!(master_anchor_channel(&seq), 52);
        }

        #[test]
        fn master_anchor_falls_back_to_most_frequent_when_slot0_null() {
            // If slot 0 is null, the anchor is the most-frequent non-zero
            // channel (the "dominant" one).
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [0, 0]; // slot 0 null
            seq[1] = [44, 0x80];
            seq[2] = [44, 0x80];
            seq[3] = [44, 0x80];
            seq[4] = [149, 0x80];
            assert_eq!(master_anchor_channel(&seq), 44);
        }

        #[test]
        fn master_anchor_is_zero_for_all_null_sequence() {
            let seq = [[0u8, 0u8]; CHANSEQ_LEN];
            assert_eq!(master_anchor_channel(&seq), 0);
        }

        #[test]
        fn master_anchor_falls_back_to_most_frequent_tie_break_lowest() {
            // On a tie in frequency, pick the lowest channel number for
            // determinism.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [0, 0];
            seq[1] = [149, 0x80];
            seq[2] = [149, 0x80];
            seq[3] = [44, 0x80];
            seq[4] = [44, 0x80];
            assert_eq!(master_anchor_channel(&seq), 44);
        }

        #[test]
        fn rebroadcast_set_includes_non_social_anchor() {
            // Anchor 52 is not a social channel → the rebroadcast set grows to
            // include it alongside 6/44/149.
            let set = rebroadcast_channels(52);
            assert!(set.contains(&6));
            assert!(set.contains(&44));
            assert!(set.contains(&149));
            assert!(set.contains(&52), "anchor 52 must be in the rebroadcast set");
        }

        #[test]
        fn rebroadcast_set_unchanged_when_anchor_is_social() {
            // If the anchor is already 6/44/149, the set is unchanged (no
            // duplicate).
            assert_eq!(rebroadcast_channels(44), vec![6, 44, 149]);
            assert_eq!(rebroadcast_channels(6), vec![6, 44, 149]);
            assert_eq!(rebroadcast_channels(149), vec![6, 44, 149]);
        }

        #[test]
        fn rebroadcast_set_for_zero_anchor_is_socials_only() {
            // No master adopted (anchor 0): just the three socials.
            assert_eq!(rebroadcast_channels(0), vec![6, 44, 149]);
        }

        #[test]
        fn replace_no_ir_slots_rewrites_dfs_to_social() {
            // A master may anchor/hop on No-IR/DFS channels (52, 60, ...) the
            // carl9170 can't TX on. We adopt it for timing but must never tune
            // the monitor there: every No-IR slot is rewritten to a TX-capable
            // social channel; TX-capable and null slots are left alone.
            let mut seq = [[0u8, 0]; CHANSEQ_LEN];
            seq[0] = [52, 0x80]; // No-IR
            seq[2] = [44, 0x80]; // TX-capable, keep
            seq[3] = [60, 0x80]; // No-IR
            seq[8] = [6, 0x51]; // TX-capable, keep

            let replaced = replace_no_ir_slots(&mut seq);

            assert_eq!(replaced, vec![52, 60]); // both No-IR channels reported
            assert!(
                seq.iter().all(|s| !crate::channel::is_no_ir_channel(s[0])),
                "no slot may remain a No-IR channel"
            );
            assert_eq!(seq[2][0], 44); // untouched
            assert_eq!(seq[8][0], 6); // untouched
            assert_eq!(seq[1][0], 0); // null left for ensure_social_coverage
            assert!(AWDL_SOCIAL_CHANNEL_NUMS.contains(&seq[0][0]));
            assert!(AWDL_SOCIAL_CHANNEL_NUMS.contains(&seq[3][0]));
        }

        #[test]
        fn spread_sends_on_every_distinct_channel_then_drops() {
            // Core regression: one mDNS announce enqueued while the master
            // chanseq = [6,44,149] is sent exactly once on each of the three
            // distinct channels as filin hops through them, then dropped.
            let mut spread = MulticastSpread::new(MULTICAST_SPREAD_CAP, HOP_CYCLE_US);
            let seq = chanseq(&[(6, 0), (44, 0), (149, 0)]);
            assert!(spread.enqueue(vec![0xAA], &seq, 44, 1_000_000));
            assert_eq!(spread.len(), 1);

            // Hop to ch6 → inject, still pending (44/149 left).
            assert_eq!(spread.drain_for_channel(6, 1_000_000), vec![vec![0xAA]]);
            assert_eq!(spread.len(), 1);
            // Hop to ch44 → inject, still pending (149 left).
            assert_eq!(spread.drain_for_channel(44, 1_000_000), vec![vec![0xAA]]);
            assert_eq!(spread.len(), 1);
            // Hop to ch149 → inject, todo now empty → entry dropped.
            assert_eq!(spread.drain_for_channel(149, 1_000_000), vec![vec![0xAA]]);
            assert!(spread.is_empty());

            // Subsequent hop to any channel yields nothing (already spread).
            assert!(spread.drain_for_channel(6, 1_000_000).is_empty());
            assert!(spread.drain_for_channel(44, 1_000_000).is_empty());
        }

        #[test]
        fn spread_dedupes_duplicate_channels_in_sequence() {
            // If the master chanseq lists the same channel on multiple slots,
            // the announce is sent once per DISTINCT channel, not once per
            // slot.
            let mut spread = MulticastSpread::new(MULTICAST_SPREAD_CAP, HOP_CYCLE_US);
            // Sequence full of 6s with a couple of 44s.
            let mut seq = [[6u8, 0u8]; CHANSEQ_LEN];
            seq[5] = [44, 0];
            seq[11] = [44, 0];
            assert!(spread.enqueue(vec![0xBB], &seq, 44, 1_000_000));

            // First time we hit ch6 → inject. Second time → nothing.
            assert_eq!(spread.drain_for_channel(6, 1_000_000), vec![vec![0xBB]]);
            assert!(spread.drain_for_channel(6, 1_000_000).is_empty());
            // ch44 → inject once, then done (todo empty).
            assert_eq!(spread.drain_for_channel(44, 1_000_000), vec![vec![0xBB]]);
            assert!(spread.is_empty());
        }

        #[test]
        fn spread_falls_back_to_single_anchor_channel() {
            // No master adopted: distinct_channels = [anchor], so the frame
            // is sent once on the anchor and then dropped — same observable
            // behaviour as the pre-spread code.
            let mut spread = MulticastSpread::new(MULTICAST_SPREAD_CAP, HOP_CYCLE_US);
            let seq = [[0u8, 0u8]; CHANSEQ_LEN]; // all null
            assert!(spread.enqueue(vec![0xCC], &seq, 44, 1_000_000));
            assert_eq!(spread.drain_for_channel(44, 1_000_000), vec![vec![0xCC]]);
            assert!(spread.is_empty());
        }

        #[test]
        fn spread_evicts_oldest_when_full() {
            // A bounded queue: when full, the oldest entry is dropped so the
            // newest announce still gets its hop cycle (no unbounded growth).
            let mut spread = MulticastSpread::new(2, HOP_CYCLE_US);
            let seq = chanseq(&[(6, 0), (44, 0)]);
            assert!(spread.enqueue(vec![0x01], &seq, 44, 1_000_000));
            assert!(spread.enqueue(vec![0x02], &seq, 44, 1_000_001));
            assert_eq!(spread.len(), 2);
            // Third enqueue evicts the first (0x01).
            assert!(spread.enqueue(vec![0x03], &seq, 44, 1_000_002));
            assert_eq!(spread.len(), 2);
            // Draining ch6 should yield only 0x02 and 0x03 (0x01 was evicted).
            let drained = spread.drain_for_channel(6, 1_000_002);
            assert_eq!(drained, vec![vec![0x02], vec![0x03]]);
        }

        #[test]
        fn spread_ages_out_stale_entries_past_one_cycle() {
            // If a frame's remaining channels never recur (e.g. the master
            // sequence changed mid-cycle), the TTL guard drops it instead of
            // leaking forever.
            let mut spread = MulticastSpread::new(MULTICAST_SPREAD_CAP, HOP_CYCLE_US);
            let seq = chanseq(&[(6, 0), (44, 0), (149, 0)]);
            assert!(spread.enqueue(vec![0xDD], &seq, 44, 1_000_000));
            // Channel 149 never recurs; advance past one hop cycle.
            let stale = 1_000_000 + HOP_CYCLE_US + 1;
            // Drain on a channel that IS in the todo: aged out → not returned.
            assert!(spread.drain_for_channel(6, stale).is_empty());
            assert!(spread.is_empty());
        }

        #[test]
        fn spread_enqueue_returns_false_when_no_target_channels() {
            // Defensive: all-zero sequence AND zero anchor → nothing to send.
            let mut spread = MulticastSpread::new(MULTICAST_SPREAD_CAP, HOP_CYCLE_US);
            let seq = [[0u8, 0u8]; CHANSEQ_LEN];
            assert!(!spread.enqueue(vec![0xEE], &seq, 0, 1_000_000));
            assert!(spread.is_empty());
        }

        // --- Bug C: transfer pin ---

        #[test]
        fn transfer_active_within_idle_timeout() {
            // Recent unicast RX from peer P on ch44 → pin to (P, 44).
            let pin = TransferPin {
                peer: [0x02, 1, 2, 3, 4, 5],
                channel: 44,
                last_rx_us: 1_000_000,
            };
            assert_eq!(
                transfer_active(Some(&pin), 1_000_000, TRANSFER_IDLE_TIMEOUT_US),
                Some(([0x02, 1, 2, 3, 4, 5], 44))
            );
        }

        #[test]
        fn transfer_releases_after_idle_timeout() {
            // Once the idle timeout elapses with no further RX, the pin
            // releases so filin can resume hopping and re-adopting.
            let pin = TransferPin {
                peer: [0x02, 1, 2, 3, 4, 5],
                channel: 44,
                last_rx_us: 1_000_000,
            };
            let after = 1_000_000 + TRANSFER_IDLE_TIMEOUT_US + 1;
            assert_eq!(
                transfer_active(Some(&pin), after, TRANSFER_IDLE_TIMEOUT_US),
                None
            );
        }

        #[test]
        fn transfer_inactive_when_no_pin_or_null_channel() {
            // No pin → not active.
            assert_eq!(transfer_active(None, 1_000_000, TRANSFER_IDLE_TIMEOUT_US), None);
            // A pin on channel 0 (null) → not active.
            let pin = TransferPin {
                peer: [0x02, 1, 2, 3, 4, 5],
                channel: 0,
                last_rx_us: 1_000_000,
            };
            assert_eq!(transfer_active(Some(&pin), 1_000_000, TRANSFER_IDLE_TIMEOUT_US), None);
        }

        // --- Bug B: master-adoption hysteresis ---

        const COMMITTED: [u8; 6] = [0x02, 0xaa, 0, 0, 0, 1];
        const CHALLENGER: [u8; 6] = [0x02, 0xbb, 0, 0, 0, 2];

        #[test]
        fn hysteresis_single_challenger_frame_does_not_flip_master() {
            // A single challenger win must NOT flip the committed master.
            let since = 1_000_000;
            let now = since + 1; // far less than the debounce window
            assert_eq!(
                master_adoption_decision(
                    COMMITTED,
                    CHALLENGER,
                    CHALLENGER,
                    since,
                    now,
                    MASTER_DEBOUNCE_US
                ),
                MasterAdoption::Keep
            );
        }

        #[test]
        fn hysteresis_sustained_challenger_wins_flip_master() {
            // A challenger that has won for the whole debounce window does
            // flip the committed master.
            let since = 1_000_000;
            let now = since + MASTER_DEBOUNCE_US;
            assert_eq!(
                master_adoption_decision(
                    COMMITTED,
                    CHALLENGER,
                    CHALLENGER,
                    since,
                    now,
                    MASTER_DEBOUNCE_US
                ),
                MasterAdoption::Adopt
            );
        }

        #[test]
        fn hysteresis_re_adopting_committed_master_is_noop_adopt() {
            // The winner being the already-committed master is an immediate
            // Adopt (refresh, clears the pending challenger).
            assert_eq!(
                master_adoption_decision(
                    COMMITTED,
                    COMMITTED,
                    CHALLENGER,
                    1_000_000,
                    1_000_000,
                    MASTER_DEBOUNCE_US
                ),
                MasterAdoption::Adopt
            );
        }

        #[test]
        fn hysteresis_unknown_challenger_does_not_adopt_immediately() {
            // A brand-new winner that is neither committed nor the tracked
            // pending challenger must Keep (start its debounce clock, not
            // adopt on the first frame).
            assert_eq!(
                master_adoption_decision(
                    COMMITTED,
                    CHALLENGER,
                    [0x02, 0xcc, 0, 0, 0, 3], // a different pending
                    1_000_000,
                    1_000_000 + MASTER_DEBOUNCE_US,
                    MASTER_DEBOUNCE_US
                ),
                MasterAdoption::Keep
            );
        }

        // --- ensure_social_coverage: guarantee ch149 coverage ---

        #[test]
        fn social_coverage_injects_missing_149_into_null_slot() {
            // Core regression: a master chanseq of all 44s + some nulls.
            // 6 and 149 are missing. After ensure_social_coverage, all
            // three social channels appear — 149 is injected so the radio
            // actually visits it during the hop cycle.
            let mut seq = [[44u8, 0x80]; CHANSEQ_LEN];
            // Make some slots null so there's room to inject.
            seq[5] = [0, 0];
            seq[6] = [0, 0];
            let injected = ensure_social_coverage(&mut seq, 0);
            assert!(injected.contains(&6));
            assert!(injected.contains(&149));
            // 149 and 6 now appear in the sequence.
            assert!(seq.iter().any(|s| s[0] == 149));
            assert!(seq.iter().any(|s| s[0] == 6));
            assert!(seq.iter().any(|s| s[0] == 44)); // 44 still there
            // No more null slots (we filled them).
            assert!(!seq.iter().any(|s| s[0] == 0));
        }

        #[test]
        fn social_coverage_overwrites_duplicate_when_no_nulls() {
            // A master chanseq full of 44s — no nulls, no 6, no 149.
            // ensure_social_coverage must overwrite DUPLICATE 44 slots to
            // inject 6 and 149 without removing 44 entirely.
            let mut seq = [[44u8, 0x80]; CHANSEQ_LEN];
            ensure_social_coverage(&mut seq, 0);
            assert!(seq.iter().any(|s| s[0] == 149));
            assert!(seq.iter().any(|s| s[0] == 6));
            assert!(seq.iter().any(|s| s[0] == 44), "44 must survive");
        }

        #[test]
        fn social_coverage_noop_when_all_present() {
            // A sequence that already has 6, 44, and 149 → nothing injected.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [6, 0x51];
            seq[1] = [44, 0x80];
            seq[2] = [149, 0x80];
            let injected = ensure_social_coverage(&mut seq, 0);
            assert!(injected.is_empty());
        }

        #[test]
        fn social_coverage_preserves_distinct_non_social_channels() {
            // If the master has a non-social channel (e.g. 36), the inject
            // must not remove it — it should use null slots, not overwrite
            // the only occurrence of a distinct channel.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [36, 0x80]; // non-social, single occurrence
            // 5 null slots for 6, 44, 149.
            let injected = ensure_social_coverage(&mut seq, 0);
            assert_eq!(injected.len(), 3);
            assert!(seq.iter().any(|s| s[0] == 36), "ch36 must survive");
        }

        // --- Part B: denser social-channel dwell (fill nulls round-robin) ---

        #[test]
        fn social_coverage_fills_nulls_round_robin_for_dense_149() {
            // A master seq with many nulls: after ensure_social_coverage, 149
            // appears in MORE than one slot (round-robin fill starting at
            // 149), so filin dwells on 149 several times per cycle.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [44, 0x80]; // one real master channel
            // 15 nulls. ensure_social_coverage injects the missing 6/149
            // (44 already present), then fills the remaining 13 nulls
            // round-robin 149/6/44 starting at 149.
            ensure_social_coverage(&mut seq, 0);
            let count_149 = seq.iter().filter(|s| s[0] == 149).count();
            assert!(
                count_149 > 1,
                "149 must appear in >1 slot (got {})",
                count_149
            );
            // No null slots remain.
            assert!(!seq.iter().any(|s| s[0] == 0), "no nulls remain");
            // 44 (master's real channel) survives.
            assert!(seq.iter().any(|s| s[0] == 44));
        }

        #[test]
        fn social_coverage_round_robin_starts_at_149() {
            // The round-robin starts at 149 so 149 gets the most slots.
            // With exactly 3 nulls and all three socials missing, each gets
            // exactly one — but 149 is first.
            let mut seq = [[44u8, 0x80]; CHANSEQ_LEN];
            seq[0] = [0, 0];
            seq[1] = [0, 0];
            seq[2] = [0, 0];
            // 44 already present (13 slots); 6 and 149 missing → 2 injected
            // into nulls; the 3rd null filled round-robin (starts at 149).
            ensure_social_coverage(&mut seq, 0);
            // No nulls remain.
            assert!(!seq.iter().any(|s| s[0] == 0));
            // 149 present at least twice (1 injected + 1 round-robin).
            let count_149 = seq.iter().filter(|s| s[0] == 149).count();
            assert!(count_149 >= 2, "149 should get round-robin dwell");
        }

        #[test]
        fn social_coverage_does_not_touch_non_null_master_slots() {
            // Master-advertised channels in non-null slots must survive the
            // round-robin fill — only nulls are overwritten.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [36, 0x80]; // non-social master channel
            seq[5] = [149, 0x80]; // social, already present
            ensure_social_coverage(&mut seq, 0);
            assert_eq!(seq[0], [36, 0x80], "master ch36 slot untouched");
            assert_eq!(seq[5], [149, 0x80], "master ch149 slot untouched");
        }

        // --- FILIN_ANCHOR_REBROADCAST.md: anchor-biased dwell ---

        #[test]
        fn anchor_biased_fill_gives_anchor_materially_more_dwell() {
            // The live iPhone cluster's decoded sequence:
            // [52,0,44,0,0,0,0,0,6,0,44,0,...] — slot 0 = 52 (anchor).
            // After ensure_social_coverage with anchor=52, the anchor must
            // occupy materially more than ~6% of slots (the bare 1/16 it had
            // from the master). Target: at least 25% (4/16).
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [52, 0x80]; // anchor at slot 0
            seq[2] = [44, 0x80]; // some master channels
            seq[8] = [6, 0x51];
            seq[10] = [44, 0x80];
            // 12 null slots. ensure_social_coverage injects 149 (missing
            // social), then fills remaining nulls biased toward the anchor.
            ensure_social_coverage(&mut seq, 52);

            let anchor_count = seq.iter().filter(|s| s[0] == 52).count();
            assert!(
                anchor_count >= 4,
                "anchor 52 must get >=4/16 slots (got {}), was ~1/16 before",
                anchor_count
            );
            // All three social channels must still be present.
            assert!(seq.iter().any(|s| s[0] == 149));
            assert!(seq.iter().any(|s| s[0] == 6));
            assert!(seq.iter().any(|s| s[0] == 44));
            // No nulls remain.
            assert!(!seq.iter().any(|s| s[0] == 0));
        }

        #[test]
        fn anchor_biased_fill_prefers_anchor_over_social_for_nulls() {
            // With many nulls, the anchor should dominate: more anchor slots
            // than any individual social channel in the filled nulls.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [52, 0x80]; // anchor
            // 15 nulls — no other master channels.
            ensure_social_coverage(&mut seq, 52);

            let anchor_count = seq.iter().filter(|s| s[0] == 52).count();
            let ch149_count = seq.iter().filter(|s| s[0] == 149).count();
            let ch6_count = seq.iter().filter(|s| s[0] == 6).count();
            let ch44_count = seq.iter().filter(|s| s[0] == 44).count();
            assert!(
                anchor_count > ch149_count
                    && anchor_count > ch6_count
                    && anchor_count > ch44_count,
                "anchor ({} slots) must dominate socials (149:{}, 6:{}, 44:{})",
                anchor_count,
                ch149_count,
                ch6_count,
                ch44_count
            );
        }

        #[test]
        fn anchor_zero_falls_back_to_dense_149_round_robin() {
            // anchor=0 (no master) → preserve the old round-robin behavior:
            // 149 gets the most slots. Backward-compatible.
            let mut seq = [[0u8, 0u8]; CHANSEQ_LEN];
            seq[0] = [44, 0x80]; // one real master channel
            ensure_social_coverage(&mut seq, 0);
            let count_149 = seq.iter().filter(|s| s[0] == 149).count();
            assert!(count_149 > 1, "149 must still get dense dwell");
            assert!(!seq.iter().any(|s| s[0] == 0));
        }

        // --- Part A: AnnounceCache cached mDNS re-broadcast ---

        #[test]
        fn announce_cache_rebroadcast_on_social_visit() {
            // A TAP multicast frame is cached; visiting a social channel
            // re-broadcasts it.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA, 0xBB], 1_000_000);
            assert_eq!(
                cache.rebroadcast(149, 1_500_000, &AWDL_SOCIAL_CHANNEL_NUMS),
                Some(&[0xAA, 0xBB][..])
            );
        }

        #[test]
        fn announce_cache_rate_limits_within_window() {
            // A second re-broadcast on the SAME channel within the window is
            // suppressed; the rate-limit is per-channel.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            assert!(cache.rebroadcast(149, 1_500_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_some());
            assert!(
                cache.rebroadcast(149, 1_900_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_none(),
                "second send within window must be suppressed"
            );
            // Past the window it fires again.
            assert!(cache.rebroadcast(149, 2_501_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_some());
        }

        #[test]
        fn announce_cache_rate_limit_is_per_channel() {
            // Re-broadcasting on ch6 must NOT rate-limit a subsequent ch149
            // send — the limit is per-channel so a dwell on 149 is never
            // blocked by a prior ch6 send.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            assert!(cache.rebroadcast(6, 1_500_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_some());
            assert!(
                cache.rebroadcast(149, 1_500_001, &AWDL_SOCIAL_CHANNEL_NUMS).is_some(),
                "ch149 must not be rate-limited by a ch6 send"
            );
        }

        #[test]
        fn announce_cache_refresh_replaces_with_latest() {
            // A new TAP frame replaces the cached one.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            cache.refresh(vec![0xCC, 0xDD], 2_000_000);
            assert_eq!(
                cache.rebroadcast(149, 2_500_000, &AWDL_SOCIAL_CHANNEL_NUMS),
                Some(&[0xCC, 0xDD][..])
            );
        }

        #[test]
        fn announce_cache_expires_when_stale() {
            // If no refresh arrives for longer than the TTL, the cache
            // stops re-broadcasting (dead receiver no longer advertised).
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            let stale = 1_000_000 + ANNOUNCE_CACHE_TTL_US + 1;
            assert!(cache.rebroadcast(149, stale, &AWDL_SOCIAL_CHANNEL_NUMS).is_none());
            assert!(!cache.is_live(stale));
        }

        #[test]
        fn announce_cache_does_not_rebroadcast_on_non_social_channel() {
            // Only social channels (6/44/149) trigger a re-broadcast.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            assert!(cache.rebroadcast(36, 1_500_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_none());
        }

        #[test]
        fn announce_cache_rebroadcasts_on_anchor_channel_when_in_allowed_set() {
            // FILIN_ANCHOR_REBROADCAST.md: when the master anchors on a
            // non-social channel (e.g. 52), the anchor must be in the allowed
            // set so the cached announce is re-broadcast there too.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA, 0xBB], 1_000_000);
            let allowed = rebroadcast_channels(52);
            assert_eq!(cache.rebroadcast(52, 1_500_000, &allowed), Some(&[0xAA, 0xBB][..]));
        }

        #[test]
        fn announce_cache_still_rate_limits_on_anchor_channel() {
            // The per-channel rate limit must apply to the anchor channel too,
            // so filin does not flood a DFS channel.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            cache.refresh(vec![0xAA], 1_000_000);
            let allowed = rebroadcast_channels(52);
            assert!(cache.rebroadcast(52, 1_500_000, &allowed).is_some());
            assert!(
                cache.rebroadcast(52, 1_900_000, &allowed).is_none(),
                "anchor channel must be rate-limited"
            );
        }

        #[test]
        fn announce_cache_empty_returns_none() {
            // With no frame cached, nothing is ever re-broadcast.
            let mut cache =
                AnnounceCache::new(ANNOUNCE_REBROADCAST_MIN_GAP_US, ANNOUNCE_CACHE_TTL_US);
            assert!(cache.rebroadcast(149, 1_000_000, &AWDL_SOCIAL_CHANNEL_NUMS).is_none());
        }
    }
}

pub mod peers {
    //! Faithful port of owl `peers.c` / `peers.h`.
    use crate::election::{ElectionState, PeerElection};

    /// owl `AWDL_CHANSEQ_LENGTH` (owl/src/channel.h:25).
    pub const CHANSEQ_LEN: usize = 16;
    /// owl `HOST_NAME_LENGTH_MAX` (owl/src/peers.h:29).
    pub const HOST_NAME_MAX: usize = 64;
    /// owl `PEERS_DEFAULT_TIMEOUT` (owl/src/peers.c:28). The peer table stores
    /// `last_update` in microseconds (set from the RX timestamp), so the value
    /// owl labels "ms" is applied as microseconds: a peer unheard for ~2 s is
    /// dropped.
    pub const PEER_TIMEOUT_US: u64 = 2_000_000;

    /// owl `struct awdl_peer` (owl/src/peers.h:38-52).
    #[derive(Debug, Clone)]
    pub struct Peer {
        pub addr: [u8; 6],
        pub last_update_us: u64,
        pub election: ElectionState,
        /// Channel sequence, each slot a `(chan_num, opclass)` pair (opclass
        /// encoding). `owl awdl_chan` is a 2-byte union.
        pub sequence: [[u8; 2]; CHANSEQ_LEN],
        pub sync_offset_us: u64,
        pub name: String,
        pub country_code: [u8; 3],
        pub infra_addr: [u8; 6],
        pub version: u8,
        pub devclass: u8,
        pub supports_v2: bool,
        pub sent_mif: bool,
        pub is_valid: bool,
    }

    impl Peer {
        /// owl `awdl_peer_new` (owl/src/peers.c:66-81).
        pub fn new(addr: [u8; 6]) -> Self {
            Self {
                addr,
                last_update_us: 0,
                election: ElectionState::new(addr),
                sequence: [[0, 0]; CHANSEQ_LEN],
                sync_offset_us: 0,
                name: String::new(),
                country_code: [b'N', b'A', 0],
                infra_addr: [0; 6],
                version: 0,
                devclass: 0,
                supports_v2: false,
                sent_mif: false,
                is_valid: false,
            }
        }

        /// owl `awdl_peer_is_valid` (owl/src/peers.c:62-64): a peer becomes a
        /// candidate for election/data once it has sent a MIF, a version, and a
        /// device class.
        pub fn is_complete(&self) -> bool {
            self.sent_mif && self.devclass != 0 && self.version != 0
        }

        /// Read-only election view consumed by `ElectionState::run`.
        pub fn election_view(&self) -> PeerElection {
            PeerElection {
                self_addr: self.addr,
                sync_addr: self.election.sync_addr,
                master_addr: self.election.master_addr,
                height: self.election.height,
                master_metric: self.election.master_metric,
                master_counter: self.election.master_counter,
                is_valid: self.is_valid,
                anchor_channel: crate::schedule::master_anchor_channel(&self.sequence),
            }
        }
    }

    /// owl `struct awdl_peer_state` and its hashmap (owl/src/peers.c).
    #[derive(Debug, Default)]
    pub struct PeerTable {
        peers: Vec<Peer>,
    }

    impl PeerTable {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn len(&self) -> usize {
            self.peers.len()
        }

        pub fn is_empty(&self) -> bool {
            self.peers.is_empty()
        }

        pub fn get(&self, addr: &[u8; 6]) -> Option<&Peer> {
            self.peers.iter().find(|p| &p.addr == addr)
        }

        pub fn get_mut(&mut self, addr: &[u8; 6]) -> Option<&mut Peer> {
            self.peers.iter_mut().find(|p| &p.addr == addr)
        }

        /// owl `awdl_peer_add` (owl/src/peers.c:83-116): create or update the
        /// peer, refresh `last_update`, and recompute validity. Returns `true`
        /// iff the peer transitioned to valid on this call (owl fires its
        /// `peer_cb` then).
        pub fn touch(&mut self, addr: [u8; 6], now_us: u64) -> bool {
            if let Some(idx) = self.peers.iter().position(|p| p.addr == addr) {
                let peer = &mut self.peers[idx];
                let was_valid = peer.is_valid;
                peer.last_update_us = now_us;
                peer.is_valid = peer.is_complete();
                peer.is_valid && !was_valid
            } else {
                let mut peer = Peer::new(addr);
                peer.last_update_us = now_us;
                peer.is_valid = peer.is_complete();
                let became = peer.is_valid;
                self.peers.push(peer);
                became
            }
        }

        pub fn iter(&self) -> std::slice::Iter<'_, Peer> {
            self.peers.iter()
        }

        /// Election inputs for all peers (owl feeds the whole table to
        /// `awdl_election_run`).
        pub fn election_views(&self) -> Vec<PeerElection> {
            self.peers.iter().map(Peer::election_view).collect()
        }

        /// owl `awdl_peers_remove` (owl/src/peers.c:170-187): drop peers whose
        /// `last_update` is strictly older than `cutoff_us`. Returns the removed
        /// addresses (owl invokes `peer_remove_cb` for each).
        pub fn remove_stale(&mut self, cutoff_us: u64) -> Vec<[u8; 6]> {
            let mut removed = Vec::new();
            self.peers.retain(|p| {
                if p.last_update_us < cutoff_us {
                    removed.push(p.addr);
                    false
                } else {
                    true
                }
            });
            removed
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn touch_creates_then_updates_peer() {
            let mut table = PeerTable::new();
            let addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            assert!(!table.touch(addr, 1_000));
            assert_eq!(table.len(), 1);
            assert_eq!(table.get(&addr).unwrap().last_update_us, 1_000);

            assert!(!table.touch(addr, 2_000));
            assert_eq!(table.len(), 1);
            assert_eq!(table.get(&addr).unwrap().last_update_us, 2_000);
        }

        #[test]
        fn peer_becomes_valid_only_with_mif_version_and_devclass() {
            let mut table = PeerTable::new();
            let addr = [0x02, 0, 0, 0, 0, 1];
            table.touch(addr, 0);

            table.get_mut(&addr).unwrap().sent_mif = true;
            assert!(!table.touch(addr, 10)); // still missing version/devclass

            table.get_mut(&addr).unwrap().version = 0x34;
            assert!(!table.touch(addr, 20)); // still missing devclass

            table.get_mut(&addr).unwrap().devclass = 1;
            assert!(table.touch(addr, 30)); // now complete -> valid transition
            assert!(table.get(&addr).unwrap().is_valid);
        }

        #[test]
        fn remove_stale_drops_old_peers_keeps_recent() {
            let mut table = PeerTable::new();
            let old = [0x02, 0, 0, 0, 0, 1];
            let recent = [0x02, 0, 0, 0, 0, 2];
            table.touch(old, 1_000);
            table.touch(recent, 5_000);

            let removed = table.remove_stale(4_000);

            assert_eq!(removed, vec![old]);
            assert!(table.get(&old).is_none());
            assert!(table.get(&recent).is_some());
        }

        #[test]
        fn election_views_carry_validity_and_addrs() {
            let mut table = PeerTable::new();
            let addr = [0x02, 0, 0, 0, 0, 7];
            table.touch(addr, 100);
            {
                let p = table.get_mut(&addr).unwrap();
                p.sent_mif = true;
                p.version = 0x34;
                p.devclass = 1;
                p.election.master_counter = 42;
            }
            table.touch(addr, 200); // recompute validity

            let views = table.election_views();
            assert_eq!(views.len(), 1);
            assert!(views[0].is_valid);
            assert_eq!(views[0].self_addr, addr);
            assert_eq!(views[0].master_counter, 42);
        }
    }
}

pub mod tx {
    use std::collections::VecDeque;

    #[derive(Debug, PartialEq, Eq)]
    pub enum InjectError {
        WouldBlock,
        Failed,
    }

    pub trait Injector {
        fn inject(&mut self, frame: &[u8]) -> Result<(), InjectError>;
    }

    #[derive(Debug, Default)]
    pub struct TxQueue {
        pending: VecDeque<Vec<u8>>,
    }

    impl TxQueue {
        pub fn push(&mut self, frame: Vec<u8>) {
            self.pending.push_back(frame);
        }

        pub fn pending_len(&self) -> usize {
            self.pending.len()
        }

        pub fn flush<I: Injector>(&mut self, injector: &mut I) -> Result<usize, InjectError> {
            let mut sent = 0;
            while let Some(frame) = self.pending.pop_front() {
                match injector.inject(&frame) {
                    Ok(()) => sent += 1,
                    Err(InjectError::WouldBlock) => {
                        self.pending.push_front(frame);
                        return Ok(sent);
                    }
                    Err(InjectError::Failed) => {
                        self.pending.push_front(frame);
                        return Err(InjectError::Failed);
                    }
                }
            }
            Ok(sent)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct BlockingInjector {
            calls: usize,
        }

        impl Injector for BlockingInjector {
            fn inject(&mut self, _frame: &[u8]) -> Result<(), InjectError> {
                self.calls += 1;
                Err(InjectError::WouldBlock)
            }
        }

        #[test]
        fn keeps_frame_queued_when_inject_would_block() {
            let mut queue = TxQueue::default();
            queue.push(vec![1, 2, 3]);
            let mut injector = BlockingInjector { calls: 0 };

            assert_eq!(queue.flush(&mut injector), Ok(0));
            assert_eq!(injector.calls, 1);
            assert_eq!(queue.pending_len(), 1);
        }
    }
}

pub mod iface {
    /// Default introspection bind address (loopback only). See
    /// FILIN_HTTP_INTROSPECT.md.
    pub const DEFAULT_HTTP_PORT: u16 = 9930;

    /// The default introspection HTTP address: `127.0.0.1:9930`. Const so it
    /// can be used in static test literals without `"parse().unwrap()"`.
    pub const fn default_http_addr() -> std::net::SocketAddr {
        std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            DEFAULT_HTTP_PORT,
        )
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Config {
        pub monitor_iface: String,
        pub host_iface: String,
        pub anchor_channel: u16,
        pub assume_monitor: bool,
        pub pcap_path: Option<String>,
        /// Localhost introspection HTTP bind address. `None` disables the
        /// server entirely (`--no-http`).
        pub http_addr: Option<std::net::SocketAddr>,
        /// FILIN_SYNC_QUALITY.md pivot: single-channel "park" mode. When set,
        /// filin stops hopping and stays fixed on ONE channel (the stable top
        /// master's anchor), maximizing presence on the iPhone's primary
        /// channel. The carl9170 is a dedicated monitor — it doesn't share a
        /// radio with infra like a real Apple device, so hopping is only
        /// needed to time-share, which we don't need to do. AW timing is kept
        /// so announces still transmit during availability windows.
        pub park: bool,
        /// owl `-f`: disable the RSSI admission filter (accept all frames
        /// regardless of signal strength). Useful for testing; defaults to
        /// filtering ON (owl's default, FILIN_AUDIT_FIXES.md #1).
        pub disable_rssi_filter: bool,
        /// `--force-master`: when `Some(metric)`, filin advertises that metric
        /// as its election `(self_counter, self_metric)` and never adopts a
        /// peer, forcing the AWDL cluster to elect filin as master and follow
        /// its social-channel sequence (pulling Apple peers off DFS/No-IR
        /// channels like 52). `None` = normal election. The contained value is
        /// the metric to advertise (see [`crate::election::force_self_master`]).
        pub force_master: Option<u32>,
        /// `--tx-retransmits N`: blindly re-inject each UNICAST data frame this
        /// many extra times (with the 802.11 Retry bit set so the peer de-dups),
        /// compensating for the carl9170's lack of MAC-layer retransmission on
        /// monitor injection (confirmed: 0 hardware retries). 0 = off (single
        /// send). Default 2 → each unicast frame sent 1+2 = 3×.
        pub tx_retransmits: u32,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ConfigError {
        MissingMonitorInterface,
        UnsupportedAnchorChannel(u16),
        NonLoopbackHttpAddr(std::net::SocketAddr),
    }

    pub fn validate_config(config: Config) -> Result<Config, ConfigError> {
        if config.monitor_iface.is_empty() {
            return Err(ConfigError::MissingMonitorInterface);
        }
        match config.anchor_channel {
            6 | 44 | 149 => {}
            other => return Err(ConfigError::UnsupportedAnchorChannel(other)),
        }
        // The introspection server must never bind off-box. Reject any non-
        // loopback address from --http-addr at config time (defense-in-depth
        // alongside the per-connection peer check in introspect::spawn).
        if let Some(addr) = config.http_addr {
            if !addr.ip().is_loopback() {
                return Err(ConfigError::NonLoopbackHttpAddr(addr));
            }
        }
        Ok(config)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn accepts_default_awdl0_channel_44_config() {
            let config = Config {
                monitor_iface: "wlan0mon".into(),
                host_iface: "awdl0".into(),
                anchor_channel: 44,
                assume_monitor: true,
                pcap_path: None,
                http_addr: Some(default_http_addr()),
                park: false,
                disable_rssi_filter: false,
                force_master: None,
                tx_retransmits: 2,
            };

            assert_eq!(validate_config(config.clone()), Ok(config));
        }
    }
}

pub mod pcap {
    use std::io::{self, Write};

    pub const DLT_IEEE802_11_RADIOTAP: u32 = 127;

    pub struct Writer<W: Write> {
        inner: W,
    }

    impl<W: Write> Writer<W> {
        pub fn new(inner: W) -> io::Result<Self> {
            let mut writer = Self { inner };
            writer.write_global_header()?;
            Ok(writer)
        }

        pub fn write_packet(&mut self, ts_us: u64, frame: &[u8]) -> io::Result<()> {
            let ts_sec = (ts_us / 1_000_000) as u32;
            let ts_usec = (ts_us % 1_000_000) as u32;
            let len = frame.len() as u32;
            self.inner.write_all(&ts_sec.to_le_bytes())?;
            self.inner.write_all(&ts_usec.to_le_bytes())?;
            self.inner.write_all(&len.to_le_bytes())?;
            self.inner.write_all(&len.to_le_bytes())?;
            self.inner.write_all(frame)
        }

        fn write_global_header(&mut self) -> io::Result<()> {
            self.inner.write_all(&0xa1b2c3d4u32.to_le_bytes())?;
            self.inner.write_all(&2u16.to_le_bytes())?;
            self.inner.write_all(&4u16.to_le_bytes())?;
            self.inner.write_all(&0i32.to_le_bytes())?;
            self.inner.write_all(&0u32.to_le_bytes())?;
            self.inner.write_all(&65_535u32.to_le_bytes())?;
            self.inner.write_all(&DLT_IEEE802_11_RADIOTAP.to_le_bytes())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn writes_little_endian_radiotap_pcap() {
            let mut bytes = Vec::new();
            {
                let mut writer = Writer::new(&mut bytes).expect("global header written");
                writer
                    .write_packet(1_234_567, &[0x00, 0x00, 0x08, 0x00, 0xd0])
                    .expect("packet written");
            }

            assert_eq!(&bytes[0..4], &[0xd4, 0xc3, 0xb2, 0xa1]);
            assert_eq!(u16::from_le_bytes(bytes[4..6].try_into().unwrap()), 2);
            assert_eq!(u16::from_le_bytes(bytes[6..8].try_into().unwrap()), 4);
            assert_eq!(
                u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                65_535
            );
            assert_eq!(
                u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
                DLT_IEEE802_11_RADIOTAP
            );
            assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
            assert_eq!(
                u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
                234_567
            );
            assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 5);
            assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 5);
            assert_eq!(&bytes[40..45], &[0x00, 0x00, 0x08, 0x00, 0xd0]);
        }
    }
}

pub mod rx {
    use crate::{awdl, ieee80211, radiotap, sync::TsftBridge};

    /// owl `RSSI_THRESHOLD_DEFAULT` (owl/src/state.h:32). Unknown peers must
    /// meet this to be discovered.
    pub const RSSI_THRESHOLD_DEFAULT: i8 = -65;

    /// owl `RSSI_GRACE_DEFAULT` (owl/src/state.h:33). Once known, a peer is
    /// admitted down to `threshold + grace` (-65 + -5 = -70) — the hysteresis
    /// that prevents master flapping (FILIN_AUDIT_FIXES.md #1, book ch.5).
    pub const RSSI_GRACE_DEFAULT: i8 = -5;

    /// Pure RSSI admission decision (owl `rx.c:273-278`). Returns `true` if
    /// the frame should be ADMITTED (peer updated / election run), `false` if
    /// it should be dropped as too weak.
    ///
    /// - Unknown peer: admit iff `rssi >= threshold` (-65).
    /// - Known peer: admit iff `rssi >= threshold + grace` (-70). The grace
    ///   gives the current master / peers slack so transient fading doesn't
    ///   cause them to drop out and trigger re-election thrash.
    /// - No RSSI: always admit (can't decide).
    pub fn rssi_admits(rssi: Option<i8>, peer_known: bool, threshold: i8, grace: i8) -> bool {
        let rssi = match rssi {
            Some(r) => r,
            None => return true,
        };
        let effective_threshold = if peer_known {
            threshold.saturating_add(grace)
        } else {
            threshold
        };
        rssi >= effective_threshold
    }


    #[derive(Debug, PartialEq, Eq)]
    pub struct ParsedActionFrame<'a> {
        pub rx_time_us: u64,
        pub rssi_dbm: Option<i8>,
        pub source: [u8; 6],
        pub destination: [u8; 6],
        pub action: awdl::ActionFrame<'a>,
        pub tlvs: Vec<awdl::Tlv<'a>>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ParseError {
        Radiotap(radiotap::RadiotapError),
        BadFcs,
        FcsTooShort,
        Ieee80211(ieee80211::ParseError),
        AwdlAction(awdl::ParseError),
        AwdlTlv(awdl::ParseError),
    }

    const RADIOTAP_F_FCS: u8 = 0x10;
    const RADIOTAP_F_BADFCS: u8 = 0x40;

    pub fn parse_action_frame<'a>(
        frame: &'a [u8],
        host_now_us: u64,
        bridge: &mut TsftBridge,
    ) -> Result<ParsedActionFrame<'a>, ParseError> {
        let radiotap = radiotap::parse_header(frame).map_err(ParseError::Radiotap)?;
        let ieee80211_payload = ieee80211_payload_without_fcs(&radiotap)?;
        let mac = ieee80211::parse_mac_header(ieee80211_payload).map_err(ParseError::Ieee80211)?;
        let action = awdl::parse_action_frame(mac.body).map_err(ParseError::AwdlAction)?;
        let tlvs = awdl::parse_tlvs(action.tlvs).map_err(ParseError::AwdlTlv)?;
        let rx_time_us = radiotap
            .tsft
            .map(|hw_tsft| bridge.timestamp_rx(hw_tsft, host_now_us))
            .unwrap_or(host_now_us);

        Ok(ParsedActionFrame {
            rx_time_us,
            rssi_dbm: radiotap.antenna_signal_dbm,
            source: mac.source,
            destination: mac.destination,
            action,
            tlvs,
        })
    }

    pub fn ieee80211_payload_without_fcs<'a>(
        radiotap: &radiotap::RadiotapHeader<'a>,
    ) -> Result<&'a [u8], ParseError> {
        let flags = radiotap.flags.unwrap_or(0);
        if flags & RADIOTAP_F_BADFCS != 0 {
            return Err(ParseError::BadFcs);
        }
        if flags & RADIOTAP_F_FCS != 0 {
            return radiotap
                .payload
                .get(
                    ..radiotap
                        .payload
                        .len()
                        .checked_sub(4)
                        .ok_or(ParseError::FcsTooShort)?,
                )
                .ok_or(ParseError::FcsTooShort);
        }
        Ok(radiotap.payload)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_radiotap_80211_awdl_action_frame_with_tsft() {
            let mut frame = vec![
                0x00, 0x00, 0x12, 0x00, // radiotap version, pad, len
                0x23, 0x00, 0x00, 0x00, // TSFT, flags, RSSI
                0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // TSFT 1000 us
                0x00, // flags
                0xd8, // -40 dBm
            ];
            frame.extend_from_slice(&[
                0xd0, 0x00, 0x00, 0x00, // action header
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination
                0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, // source
                0x00, 0x25, 0x00, 0xff, 0x94, 0x73, // bssid
                0x00, 0x00, // sequence control
                0x7f, 0x00, 0x17, 0xf2, 0x08, 0x10, 0x03, 0x00, // AWDL MIF
                0x00, 0x00, 0x00, 0x00, // phy_tx
                0x00, 0x00, 0x00, 0x00, // target_tx
                0x04, 0x02, 0x00, 0xaa, 0xbb, // TLV
            ]);

            let mut bridge = TsftBridge::default();
            let parsed = parse_action_frame(&frame, 101_000, &mut bridge)
                .expect("valid RX AWDL action frame");

            assert_eq!(parsed.rx_time_us, 101_000);
            assert_eq!(parsed.rssi_dbm, Some(-40));
            assert_eq!(parsed.source, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            assert_eq!(parsed.destination, [0xff; 6]);
            assert_eq!(parsed.action.subtype, awdl::ActionSubtype::Mif);
            assert_eq!(parsed.tlvs[0].kind, 0x04);
        }

        #[test]
        fn falls_back_to_host_time_when_radiotap_has_no_tsft() {
            let mut frame = vec![
                0x00, 0x00, 0x12, 0x00, // radiotap version, pad, len
                0x2e, 0x48, 0x00, 0x00, // flags, rate, channel, signal, antenna, rx flags
                0x00, // flags
                0x0c, // rate
                0x64, 0x14, 0x40, 0x01, // channel frequency 5220 + flags
                0xc3, // -61 dBm
                0x05, // antenna
                0x00, 0x00, // RX flags
            ];
            frame.extend_from_slice(&[
                0xd0, 0x00, 0x00, 0x00, // action header
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination
                0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, // source
                0x00, 0x25, 0x00, 0xff, 0x94, 0x73, // bssid
                0x00, 0x00, // sequence control
                0x7f, 0x00, 0x17, 0xf2, 0x08, 0x10, 0x03, 0x00, // AWDL MIF
                0x00, 0x00, 0x00, 0x00, // phy_tx
                0x00, 0x00, 0x00, 0x00, // target_tx
                0x04, 0x02, 0x00, 0xaa, 0xbb, // TLV
            ]);

            let mut bridge = TsftBridge::default();
            let parsed = parse_action_frame(&frame, 202_000, &mut bridge)
                .expect("valid RX AWDL action frame without TSFT");

            assert_eq!(parsed.rx_time_us, 202_000);
            assert_eq!(parsed.rssi_dbm, Some(-61));
            assert_eq!(parsed.action.subtype, awdl::ActionSubtype::Mif);
        }

        #[test]
        fn strips_radiotap_fcs_before_parsing_action_tlvs() {
            let mut frame = vec![
                0x00, 0x00, 0x0a, 0x00, // radiotap version, pad, len
                0x02, 0x00, 0x00, 0x00, // present: flags
                0x10, // flags: frame includes trailing FCS
                0x00, // pad to radiotap len
            ];
            frame.extend_from_slice(&[
                0xd0, 0x00, 0x00, 0x00, // action header
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination
                0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, // source
                0x00, 0x25, 0x00, 0xff, 0x94, 0x73, // bssid
                0x00, 0x00, // sequence control
                0x7f, 0x00, 0x17, 0xf2, 0x08, 0x10, 0x03, 0x00, // AWDL MIF
                0x00, 0x00, 0x00, 0x00, // phy_tx
                0x00, 0x00, 0x00, 0x00, // target_tx
                0x04, 0x02, 0x00, 0xaa, 0xbb, // one TLV
                0xde, 0xad, 0xbe, 0xef, // trailing FCS
            ]);

            let mut bridge = TsftBridge::default();
            let parsed = parse_action_frame(&frame, 303_000, &mut bridge)
                .expect("valid RX AWDL action frame with trailing FCS");

            assert_eq!(parsed.tlvs.len(), 1);
            assert_eq!(parsed.tlvs[0].kind, 0x04);
            assert_eq!(parsed.tlvs[0].value, &[0xaa, 0xbb]);
        }

        #[test]
        fn rejects_radiotap_bad_fcs_before_action_parse() {
            let mut frame = vec![
                0x00, 0x00, 0x0a, 0x00, // radiotap version, pad, len
                0x02, 0x00, 0x00, 0x00, // present: flags
                0x40, // flags: bad FCS
                0x00, // pad to radiotap len
            ];
            frame.extend_from_slice(&[
                0xd0, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02, 0xaa, 0xbb, 0xcc,
                0xdd, 0xee, 0x00, 0x25, 0x00, 0xff, 0x94, 0x73, 0x00, 0x00,
            ]);

            let mut bridge = TsftBridge::default();
            let err =
                parse_action_frame(&frame, 303_000, &mut bridge).expect_err("bad FCS rejected");

            assert_eq!(err, ParseError::BadFcs);
        }

        // --- RSSI admission filter (FILIN_AUDIT_FIXES.md #1 / owl rx.c:273-278) ---

        #[test]
        fn rssi_admits_unknown_peer_at_threshold() {
            // Unknown peer at exactly -65 dBm → admitted (>= threshold).
            assert!(rssi_admits(Some(-65), false, -65, -5));
        }

        #[test]
        fn rssi_drops_unknown_peer_below_threshold() {
            // Unknown peer at -66 → dropped (< -65).
            assert!(!rssi_admits(Some(-66), false, -65, -5));
        }

        #[test]
        fn rssi_admits_known_peer_with_grace_below_threshold() {
            // Known peer at -70 → admitted: threshold + grace = -65 + -5 = -70.
            assert!(rssi_admits(Some(-70), true, -65, -5));
        }

        #[test]
        fn rssi_drops_known_peer_below_grace_threshold() {
            // Known peer at -71 → dropped (< -70).
            assert!(!rssi_admits(Some(-71), true, -65, -5));
        }

        #[test]
        fn rssi_admits_when_no_rssi_available() {
            // No RSSI info → don't filter (can't make a decision).
            assert!(rssi_admits(None, false, -65, -5));
            assert!(rssi_admits(None, true, -65, -5));
        }

        #[test]
        fn rssi_strong_signal_always_admitted() {
            // Strong signal (-40) always admitted regardless of known/unknown.
            assert!(rssi_admits(Some(-40), false, -65, -5));
            assert!(rssi_admits(Some(-40), true, -65, -5));
        }
    }
}

pub mod cli {
    use crate::iface;
    use clap::Parser;

    #[derive(Debug, PartialEq, Eq)]
    pub enum CliError {
        Invalid,
    }

    #[derive(Debug, Parser)]
    #[command(name = "filin", disable_help_flag = true)]
    struct Args {
        #[arg(long = "help", action = clap::ArgAction::Help, help = "Print help")]
        _help: Option<bool>,
        #[arg(short = 'i')]
        monitor_iface: String,
        #[arg(short = 'c', default_value_t = 44)]
        anchor_channel: u16,
        #[arg(short = 'h', default_value = "awdl0")]
        host_iface: String,
        /// Assume the monitor interface is already in monitor mode: SKIP filin's
        /// monitor-mode configuration (down → type monitor). filin still brings
        /// the iface up and sets the channel. Without -N (default), filin forces
        /// monitor mode so it auto-recovers a card that re-enumerated in managed
        /// mode after a USB bounce.
        #[arg(short = 'N', long = "assume-monitor")]
        assume_monitor: bool,
        #[arg(long = "pcap")]
        pcap_path: Option<String>,
        /// Localhost HTTP/JSON introspection bind address (default 127.0.0.1:9930).
        #[arg(
            long = "http-addr",
            default_value = "127.0.0.1:9930",
            help = "Localhost HTTP introspection address (loopback only)"
        )]
        http_addr: std::net::SocketAddr,
        /// Disable the introspection HTTP server entirely.
        #[arg(long = "no-http")]
        no_http: bool,
        /// Single-channel park mode: stop hopping, stay fixed on the stable
        /// top master's anchor channel. Opt-in — default is the hopping mode.
        #[arg(long = "park")]
        park: bool,
        /// Disable the RSSI admission filter (accept all frames regardless of
        /// signal strength). Equivalent to owl's `-f`.
        #[arg(short = 'f', long = "no-rssi-filter")]
        disable_rssi_filter: bool,
        /// Force filin to win the AWDL master election: advertise an inflated
        /// election metric and never adopt a peer, so the cluster follows
        /// filin's social-channel sequence (pulls Apple peers off DFS/No-IR
        /// channels such as 52). ON BY DEFAULT — this flag is now redundant
        /// (kept for backward compatibility) but still forces it on even if
        /// `--no-force-master` is also given. Use the non-DFS anchor via `-c`.
        #[arg(short = 'M', long = "force-master")]
        force_master: bool,
        /// Opt out of force-master mode: run the normal AWDL election (adopt the
        /// best peer as master). Overridden by an explicit `-M/--force-master`.
        #[arg(long = "no-force-master")]
        no_force_master: bool,
        /// Override the election metric/counter advertised in force-master mode
        /// (default `DEFAULT_FORCE_MASTER_METRIC`). Raise it if a cluster
        /// refuses to switch. Ignored when `--no-force-master` is in effect.
        #[arg(long = "force-master-metric")]
        force_master_metric: Option<u32>,
        /// Blind redundancy: re-inject each unicast data frame this many EXTRA
        /// times (with the 802.11 Retry bit) to survive RF loss without MAC
        /// retransmission. Default 0 (OFF) — on the carl9170, even 2x extra
        /// during a high-rate upload overloads the half-duplex channel / inject
        /// queue and WEDGES the transfer (airtime contention, not raw loss, is
        /// the bottleneck). Enable cautiously, low values only.
        #[arg(long = "tx-retransmits", default_value_t = 0)]
        tx_retransmits: u32,
        /// Preflight only: probe the `-i` adapter's capabilities (does its
        /// driver support monitor mode?), print a verdict, and exit WITHOUT
        /// bringing any link up. Needs no root. Useful for checking whether a
        /// given Wi-Fi adapter can be used as filin's radio.
        #[arg(long = "check")]
        check: bool,
    }

    pub fn parse_config_from_env() -> Result<(iface::Config, bool), CliError> {
        let args = Args::parse();
        let check = args.check;
        let config = config_from_args(args)?;
        Ok((config, check))
    }

    pub fn parse_config_from<I, S>(args: I) -> Result<iface::Config, CliError>
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString> + Clone,
    {
        let args = Args::try_parse_from(args).map_err(|_| CliError::Invalid)?;
        config_from_args(args)
    }

    fn config_from_args(args: Args) -> Result<iface::Config, CliError> {
        let http_addr = if args.no_http { None } else { Some(args.http_addr) };
        iface::validate_config(iface::Config {
            monitor_iface: args.monitor_iface,
            host_iface: args.host_iface,
            anchor_channel: args.anchor_channel,
            assume_monitor: args.assume_monitor,
            pcap_path: args.pcap_path.or_else(|| std::env::var("FILIN_PCAP").ok()),
            http_addr,
            park: args.park,
            disable_rssi_filter: args.disable_rssi_filter,
            // Force-master is ON BY DEFAULT. `--no-force-master` opts out, but
            // an explicit `-M/--force-master` always wins (forces it on).
            force_master: if args.force_master || !args.no_force_master {
                Some(
                    args.force_master_metric
                        .unwrap_or(crate::election::DEFAULT_FORCE_MASTER_METRIC),
                )
            } else {
                None
            },
            tx_retransmits: args.tx_retransmits,
        })
        .map_err(|_| CliError::Invalid)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_owl_compatible_interface_flags() {
            let config =
                parse_config_from(["filin", "-i", "wlan0mon", "-c", "44", "-h", "awdl0", "-N"])
                    .expect("valid CLI args");

            assert_eq!(
                config,
                iface::Config {
                    monitor_iface: "wlan0mon".into(),
                    host_iface: "awdl0".into(),
                    anchor_channel: 44,
                    assume_monitor: true,
                    pcap_path: None,
                    http_addr: Some(iface::default_http_addr()),
                    park: false,
                disable_rssi_filter: false,
                    // Force-master is on by default now.
                    force_master: Some(crate::election::DEFAULT_FORCE_MASTER_METRIC),
                    tx_retransmits: 0,
                }
            );
        }

        #[test]
        fn parses_ignored_frame_pcap_path() {
            let config = parse_config_from([
                "filin",
                "-i",
                "wlan0mon",
                "-h",
                "awdl0",
                "--pcap",
                "/tmp/filin-ignored.pcap",
            ])
            .expect("valid CLI args");

            assert_eq!(config.pcap_path, Some("/tmp/filin-ignored.pcap".into()));
        }

        #[test]
        fn supports_long_help_without_reclaiming_short_h() {
            let err = Args::try_parse_from(["filin", "--help"]).expect_err("help exits parser");

            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
            assert!(err.to_string().contains("Usage:"));

            let config = parse_config_from(["filin", "-i", "wlan0mon", "-h", "awdl-test"])
                .expect("short -h remains host iface");
            assert_eq!(config.host_iface, "awdl-test");
        }

        #[test]
        fn http_addr_defaults_to_loopback_9930() {
            let config = parse_config_from(["filin", "-i", "wlan0mon"])
                .expect("valid CLI args");
            assert_eq!(config.http_addr, Some(iface::default_http_addr()));
            let addr = config.http_addr.expect("some");
            assert!(addr.ip().is_loopback());
            assert_eq!(addr.port(), iface::DEFAULT_HTTP_PORT);
        }

        #[test]
        fn http_addr_flag_overrides_the_default() {
            let config = parse_config_from([
                "filin",
                "-i",
                "wlan0mon",
                "--http-addr",
                "127.0.0.1:9999",
            ])
            .expect("valid CLI args");
            let addr = config.http_addr.expect("some");
            assert!(addr.ip().is_loopback());
            assert_eq!(addr.port(), 9999);
        }

        #[test]
        fn force_master_defaults_on() {
            let config = parse_config_from(["filin", "-i", "wlan0mon"])
                .expect("valid CLI args");
            assert_eq!(
                config.force_master,
                Some(crate::election::DEFAULT_FORCE_MASTER_METRIC)
            );
        }

        #[test]
        fn no_force_master_flag_opts_out() {
            let config = parse_config_from(["filin", "-i", "wlan0mon", "--no-force-master"])
                .expect("valid CLI args");
            assert_eq!(config.force_master, None);
        }

        #[test]
        fn explicit_force_master_overrides_opt_out() {
            let config =
                parse_config_from(["filin", "-i", "wlan0mon", "--no-force-master", "-M"])
                    .expect("valid CLI args");
            assert_eq!(
                config.force_master,
                Some(crate::election::DEFAULT_FORCE_MASTER_METRIC)
            );
        }

        #[test]
        fn force_master_metric_override_applies_by_default() {
            let with = parse_config_from([
                "filin", "-i", "wlan0mon", "--force-master-metric", "123456",
            ])
            .expect("valid CLI args");
            assert_eq!(with.force_master, Some(123456));

            // Opted out: the metric override is ignored.
            let without = parse_config_from([
                "filin", "-i", "wlan0mon", "--no-force-master", "--force-master-metric", "123456",
            ])
            .expect("valid CLI args");
            assert_eq!(without.force_master, None);
        }

        #[test]
        fn no_http_flag_disables_introspection_server() {
            let config = parse_config_from(["filin", "-i", "wlan0mon", "--no-http"])
                .expect("valid CLI args");
            assert_eq!(config.http_addr, None);
        }

        #[test]
        fn validate_rejects_non_loopback_http_addr() {
            let non_loopback: std::net::SocketAddr = "10.0.0.5:9930".parse().unwrap();
            let bad = iface::Config {
                monitor_iface: "wlan0mon".into(),
                host_iface: "awdl0".into(),
                anchor_channel: 44,
                assume_monitor: true,
                pcap_path: None,
                http_addr: Some(non_loopback),
                park: false,
                disable_rssi_filter: false,
                force_master: None,
                tx_retransmits: 2,
            };
            assert_eq!(
                iface::validate_config(bad),
                Err(iface::ConfigError::NonLoopbackHttpAddr(non_loopback))
            );
        }

        #[test]
        fn park_flag_defaults_off() {
            let config = parse_config_from(["filin", "-i", "wlan0mon"]).expect("valid");
            assert!(!config.park, "park defaults off — hopping is the default");
        }

        #[test]
        fn park_flag_opts_into_single_channel_mode() {
            let config =
                parse_config_from(["filin", "-i", "wlan0mon", "--park"]).expect("valid");
            assert!(config.park, "--park enables single-channel park mode");
        }
    }
}

pub mod state {
    //! Faithful port of owl `state.c` / `state.h`: aggregates the node's
    //! election, sync, channel, and peer-table state and applies received
    //! action-frame TLVs the way owl's `awdl_rx_action` does.
    use crate::{
        awdl,
        election::ElectionState,
        peers::{self, Peer, PeerTable, CHANSEQ_LEN},
        sync::SyncState,
    };

    /// owl `awdl_version(3, 4)` (owl/src/state.c:34).
    pub const AWDL_VERSION: u8 = 0x34;
    /// owl `AWDL_DEVCLASS_MACOS` (owl/src/version.h:34).
    pub const AWDL_DEVCLASS_MACOS: u8 = 1;

    /// owl `struct awdl_state` (owl/src/state.h:50-85), minus I/O callbacks.
    pub struct AwdlState {
        pub self_addr: [u8; 6],
        pub name: String,
        pub version: u8,
        pub dev_class: u8,
        pub election: ElectionState,
        pub sync: SyncState,
        pub peers: PeerTable,
        pub anchor_channel: u16,
        /// Own channel sequence as `(chan_num, opclass)` pairs. owl fills this
        /// with the master channel via `awdl_chanseq_init_static`. When filin
        /// syncs to a master, it adopts the master's sequence so they hop in
        /// lockstep.
        pub channel_sequence: [[u8; 2]; CHANSEQ_LEN],
        /// owl `state->channel.current` — the channel the monitor is currently
        /// tuned to (0 = not yet set / null slot).
        pub current_channel: u8,
        pub tx_seq: u16,
        /// The master whose channel sequence filin has committed to (Bug B
        /// hysteresis). Re-adoption only happens when a challenger wins the
        /// election for [`crate::schedule::MASTER_DEBOUNCE_US`].
        pub committed_master: [u8; 6],
        /// Challenger that won the most recent election but has not yet
        /// passed the debounce window (Bug B).
        pub pending_master: [u8; 6],
        /// When [`pending_master`] first won the election (Bug B).
        pub pending_since_us: u64,
        /// Sticky transfer pin: the peer/channel of the most recent unicast
        /// data RX (Bug C). While active, filin holds that channel and
        /// suppresses hopping + master re-adoption so a bulk AirDrop upload
        /// is not cut off mid-stream.
        pub transfer: Option<crate::schedule::TransferPin>,
        /// The adopted master's anchor channel (slot-0 / dominant channel of
        /// the decoded master sequence). Used to re-broadcast the cached
        /// announce AND bias dwell toward it. 0 when self-master or no master.
        pub master_anchor: u8,
        /// Monotonic time (us) when the current committed master was adopted.
        /// Drives `/status master_age_ms` (FILIN_SYNC_QUALITY.md Part A) and
        /// is bumped on every real master switch.
        pub master_adopted_at_us: u64,
        /// Most recent sync error (TU) measured against the elected master's
        /// sync TLV, or `None` if no sync update has been processed yet. Read
        /// by the runtime to feed `/status aw_alignment_pct`
        /// (FILIN_SYNC_QUALITY.md Part A).
        pub last_sync_error_tu: Option<i64>,
        /// Last-seen cluster top master (election.master_addr), used by
        /// [`note_election_transitions`] to detect true cluster churn vs
        /// re-parenting. FILIN_SYNC_QUALITY.md.
        last_seen_master_addr: [u8; 6],
        /// Last-seen direct parent (election.sync_addr), used by
        /// [`note_election_transitions`] to detect re-parenting.
        last_seen_sync_addr: [u8; 6],
        /// FILIN_SYNC_QUALITY.md pivot: when `Some`, filin is in single-channel
        /// park mode — `desired_channel` returns this channel for every EAW
        /// slot and `update_channel` never hops. Set via [`enable_park`].
        park_channel: Option<u8>,
    }

    impl AwdlState {
        /// owl `awdl_init_state` (owl/src/state.c:30-67).
        pub fn new(
            self_addr: [u8; 6],
            name: String,
            anchor_channel: u16,
            now_us: u64,
        ) -> Self {
            let opclass = chan_opclass(anchor_channel);
            Self {
                self_addr,
                name,
                version: AWDL_VERSION,
                dev_class: AWDL_DEVCLASS_MACOS,
                election: ElectionState::new(self_addr),
                sync: SyncState::new(now_us),
                peers: PeerTable::new(),
                anchor_channel,
                channel_sequence: [[anchor_channel as u8, opclass]; CHANSEQ_LEN],
                current_channel: 0,
                tx_seq: 0,
                committed_master: self_addr,
                pending_master: self_addr,
                pending_since_us: now_us,
                transfer: None,
                master_anchor: 0,
                master_adopted_at_us: now_us,
                last_sync_error_tu: None,
                last_seen_master_addr: self_addr,
                last_seen_sync_addr: self_addr,
                park_channel: None,
            }
        }

        /// Apply a received action frame: register/update the originating peer,
        /// fold every TLV into that peer's state (election, chanseq, version,
        /// …), mark MIF, gate sync to the elected master, then re-run election.
        /// Mirrors owl `awdl_rx_action` (owl/src/rx.c:253-320) and
        /// `awdl_handle_*_tlv`.
        pub fn apply_action(
            &mut self,
            src: [u8; 6],
            subtype: awdl::ActionSubtype,
            tlvs: &[awdl::Tlv<'_>],
            rx_time_us: u64,
        ) {
            // Monitor mode reflects our own injected frames back to us. owl
            // drops frames sourced from self (rx.c); filin must too, or it
            // registers its own MAC as a peer and elects itself master,
            // flapping the election and wrecking sync with real peers.
            if src == self.self_addr {
                return;
            }
            self.peers.touch(src, rx_time_us);
            let mut sync_update: Option<(u16, u16)> = None;
            if let Some(peer) = self.peers.get_mut(&src) {
                for tlv in tlvs {
                    match tlv.kind {
                        4 => {
                            if let Ok(p) = awdl::parse_sync_parameters(tlv.value) {
                                sync_update = Some((p.time_to_next_aw_tu, p.aw_counter));
                            }
                        }
                        5 if !peer.supports_v2 => apply_election_v1(peer, tlv.value),
                        18 => apply_chanseq(peer, tlv.value),
                        24 => apply_election_v2(peer, tlv.value),
                        16 => apply_arpa(peer, tlv.value),
                        21 => apply_version(peer, tlv.value),
                        _ => {}
                    }
                }
                if subtype == awdl::ActionSubtype::Mif {
                    peer.sent_mif = true;
                }
            }
            // owl only adopts sync from the elected sync master (rx.c:39).
            if let Some((time_to_next_aw_tu, aw_counter)) = sync_update {
                if self.election.is_sync_master(&src) {
                    let error_tu =
                        self.sync
                            .sync_error_tu(rx_time_us, time_to_next_aw_tu, aw_counter);
                    self.last_sync_error_tu = Some(error_tu);
                    self.sync
                        .update_from_master(rx_time_us, time_to_next_aw_tu, aw_counter);
                    tracing::trace!(
                        error_tu,
                        aw_counter,
                        time_to_next_aw_tu,
                        "sync_error"
                    );
                }
            }
            // recompute validity (owl awdl_peer_add after TLVs, rx.c:317) and
            // run election (owl awdl_clean_peers, core.c:333).
            self.peers.touch(src, rx_time_us);
            self.election.run(&self.peers.election_views());
            // Adopt the elected master's channel sequence so filin hops to the
            // same channels as the peer. When filin is self-master, keep the
            // default static anchor sequence. Hysteresis (Bug B) and the
            // active-transfer pin (Bug C) gate re-adoption.
            self.sync_channel_sequence(rx_time_us);
        }

        /// If we have an elected master with a non-trivial channel sequence,
        /// adopt it as our own. When we lose the master, revert to the static
        /// anchor sequence. This ensures filin is on the same channel as the
        /// peer during each EAW slot.
        ///
        /// Bug B (hysteresis): a challenger must win the election for
        /// [`crate::schedule::MASTER_DEBOUNCE_US`] before filin commits to its
        /// channel sequence — stops the master thrash on a contended link.
        ///
        /// Bug C (transfer pin): while a unicast transfer is active, skip
        /// re-adoption entirely so an in-flight bulk upload is not
        /// descheduled.
        fn sync_channel_sequence(&mut self, now_us: u64) {
            // Bug C: an active unicast transfer freezes the committed master
            // (and thus the channel schedule) until the peer goes quiet.
            if crate::schedule::transfer_active(
                self.transfer.as_ref(),
                now_us,
                crate::schedule::TRANSFER_IDLE_TIMEOUT_US,
            )
            .is_some()
            {
                return;
            }
            let winner = self.election.sync_addr;
            // Bug B: debounce a challenger before committing — but only when
            // SWITCHING between two real external masters. The first adoption
            // (self → real master) and master loss (real master → self) are
            // immediate: neither is a flip-flop, so neither is debounced.
            let decision = if self.committed_master == self.self_addr
                || winner == self.self_addr
            {
                crate::schedule::MasterAdoption::Adopt
            } else {
                crate::schedule::master_adoption_decision(
                    self.committed_master,
                    winner,
                    self.pending_master,
                    self.pending_since_us,
                    now_us,
                    crate::schedule::MASTER_DEBOUNCE_US,
                )
            };
            match decision {
                crate::schedule::MasterAdoption::Adopt => {
                    if self.committed_master != winner {
                        // Real master switch: stamp the adoption time so
                        // /status master_age_ms resets
                        // (FILIN_SYNC_QUALITY.md Part A).
                        self.master_adopted_at_us = now_us;
                    }
                    self.committed_master = winner;
                    self.pending_master = winner;
                    self.pending_since_us = now_us;
                }
                crate::schedule::MasterAdoption::Keep => {
                    // Start/refresh the challenger's debounce clock.
                    if winner != self.pending_master {
                        self.pending_master = winner;
                        self.pending_since_us = now_us;
                    }
                    return; // hold the currently-committed master
                }
            }
            let master = self.committed_master;
            if master == self.self_addr {
                // self-master (incl. force-master): use the static single-anchor
                // sequence. A full 6/44/149 rotation was tried but made AirDrop
                // WORSE in practice — staying parked on the one anchor keeps a
                // stronger, more continuous presence on the channel the peers
                // actually meet us on. Use `-c` to pick the anchor.
                let opclass = chan_opclass(self.anchor_channel);
                let anchor_seq = [[self.anchor_channel as u8, opclass]; CHANSEQ_LEN];
                if self.channel_sequence != anchor_seq {
                    self.channel_sequence = anchor_seq;
                    self.master_anchor = 0; // no external master
                    tracing::debug!("reverted to static anchor channel sequence");
                }
            } else if let Some(peer) = self.peers.get(&master) {
                // adopted master: copy its sequence if it has any non-null slots
                if peer.sequence.iter().any(|s| s[0] != 0)
                    && self.channel_sequence != peer.sequence
                {
                    let decoded: Vec<u8> =
                        peer.sequence.iter().map(|s| s[0]).collect();
                    self.channel_sequence = peer.sequence;
                    // We adopt this master for AW *timing*, but the carl9170
                    // can't TX/RX on its No-IR/DFS slots — rewrite those to
                    // TX-capable social channels so the monitor never tunes
                    // there and our announce still lands in-window.
                    let no_ir = crate::schedule::replace_no_ir_slots(
                        &mut self.channel_sequence,
                    );
                    // Extract the master's anchor (slot-0 / dominant channel)
                    // so filin can re-broadcast + dwell on it adaptively. Taken
                    // AFTER No-IR rewrite so the anchor is always TX-capable.
                    let anchor = crate::schedule::master_anchor_channel(
                        &self.channel_sequence,
                    );
                    self.master_anchor = anchor;
                    // Ensure filin visits ALL AWDL social channels (6, 44,
                    // 149) even if the master omits some — e.g. a master in
                    // an active data session may advertise only 44/6, but
                    // iPhones scan 149 for AirDrop discovery. Also bias null
                    // fills toward the anchor (FILIN_ANCHOR_REBROADCAST.md).
                    let injected = crate::schedule::ensure_social_coverage(
                        &mut self.channel_sequence,
                        anchor,
                    );
                    let final_seq: Vec<u8> =
                        self.channel_sequence.iter().map(|s| s[0]).collect();
                    tracing::info!(
                        master = ?master,
                        decoded = ?decoded,
                        no_ir_replaced = ?no_ir,
                        injected = ?injected,
                        final = ?final_seq,
                        "adopted master channel sequence"
                    );
                }
            }
        }

        /// owl `awdl_chan_num(state->channel.sequence[slot], enc)` — the channel
        /// number for the current EAW slot from our own sequence.
        ///
        /// Bug C: while a unicast transfer is active, returns the PINNED peer
        /// channel instead of the slot channel, so [`update_channel`] does not
        /// hop the radio away mid-upload.
        pub fn desired_channel(&self, now_us: u64) -> u8 {
            // FILIN_SYNC_QUALITY.md pivot: in park mode, return the parked
            // channel for every EAW slot — no hopping. The dedicated monitor
            // doesn't share a radio, so it need not time-share channels.
            if let Some(ch) = self.park_channel {
                return ch;
            }
            if let Some((_, channel)) = crate::schedule::transfer_active(
                self.transfer.as_ref(),
                now_us,
                crate::schedule::TRANSFER_IDLE_TIMEOUT_US,
            ) {
                return channel;
            }
            let slot = usize::from(self.sync.current_eaw(now_us) % CHANSEQ_LEN as u16);
            self.channel_sequence[slot][0]
        }

        /// Enable single-channel park mode on `channel`. Subsequent calls to
        /// [`desired_channel`] / [`update_channel`] stay fixed on it; only a
        /// different `enable_park` call (or the runtime updating the park
        /// channel to track the top master's anchor) moves the radio.
        pub fn enable_park(&mut self, channel: u8) {
            self.park_channel = Some(channel);
        }

        /// Disable park mode and resume hopping.
        pub fn disable_park(&mut self) {
            self.park_channel = None;
        }

        /// Whether park mode is currently active.
        pub fn is_parked(&self) -> bool {
            self.park_channel.is_some()
        }

        /// The channel filin should park on (FILIN_SYNC_QUALITY.md): the
        /// dominant slot-0 channel among peers reporting the elected cluster
        /// top master (`election.master_addr`). Falls back to the configured
        /// anchor channel when no such peers are known yet. This keys off
        /// master_addr (stable) NOT sync_addr (churning parent), so the park
        /// channel is stable across re-parenting.
        pub fn park_channel(&self) -> u8 {
            use std::collections::BTreeMap;
            let top = self.election.master_addr;
            let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
            for peer in self.peers.iter() {
                let ch = peer.sequence[0][0];
                // Skip No-IR/DFS anchors: the carl9170 can't TX there, so we
                // must never park there (we'd be unable to announce or answer
                // /Discover|/Ask). A No-IR-anchored cluster still visits the
                // social channels, where we CAN reach it.
                if peer.election.master_addr == top
                    && ch != 0
                    && !crate::channel::is_no_ir_channel(ch)
                {
                    *counts.entry(ch).or_default() += 1;
                }
            }
            if let Some((&ch, _)) = counts.iter().max_by_key(|(_, c)| *c) {
                return ch;
            }
            // No TX-capable slot-0 among peers reporting the top master: if the
            // top master is itself a peer and its anchor is TX-capable, use it.
            if let Some(top_peer) = self.peers.get(&top) {
                let ch = top_peer.sequence[0][0];
                if ch != 0 && !crate::channel::is_no_ir_channel(ch) {
                    return ch;
                }
            }
            // Last resort: the configured anchor channel (always TX-capable).
            self.anchor_channel as u8
        }

        /// Bug C: record a received unicast data frame from `peer` on
        /// `channel`, (re)arming the sticky transfer pin so filin holds this
        /// channel until the idle timeout elapses.
        pub fn note_unicast_rx(&mut self, peer: [u8; 6], channel: u8, now_us: u64) {
            self.transfer = Some(crate::schedule::TransferPin {
                peer,
                channel,
                last_rx_us: now_us,
            });
        }

        /// owl `awdl_switch_channel` decision (owl/daemon/core.c:279-308):
        /// returns `Some(channel)` if the monitor must be retuned, `None` if
        /// no switch is needed (same channel, or null slot).
        pub fn update_channel(&mut self, now_us: u64) -> Option<u16> {
            let desired = self.desired_channel(now_us);
            if desired == self.current_channel {
                return None;
            }
            self.current_channel = desired;
            if desired > 0 {
                Some(u16::from(desired))
            } else {
                None
            }
        }

        /// owl `awdl_clean_peers` (owl/daemon/core.c:320-336): drop stale peers
        /// then re-run election. Returns the removed peer addresses.
        pub fn clean_peers(&mut self, now_us: u64) -> Vec<[u8; 6]> {
            let cutoff = now_us.saturating_sub(peers::PEER_TIMEOUT_US);
            let removed = self.peers.remove_stale(cutoff);
            self.election.run(&self.peers.election_views());
            self.sync_channel_sequence(now_us);
            removed
        }

        /// owl `awdl_state_next_sequence_number` (owl/src/state.c:79-81).
        pub fn next_data_seq(&mut self) -> u16 {
            let s = self.tx_seq;
            self.tx_seq = self.tx_seq.wrapping_add(1);
            s
        }

        /// Compare the current election `master_addr`/`sync_addr` against the
        /// last-seen values, update the baseline, and report what changed.
        /// The runtime calls this after each `apply_action` to bump the
        /// introspection change counters. Distinguishes true cluster churn
        /// (master_addr flips) from re-parenting (sync_addr flips while
        /// master_addr stays stable) — FILIN_SYNC_QUALITY.md.
        pub fn note_election_transitions(&mut self) -> ElectionTransitions {
            let master_addr_changed = self.election.master_addr != self.last_seen_master_addr;
            let sync_addr_changed = self.election.sync_addr != self.last_seen_sync_addr;
            self.last_seen_master_addr = self.election.master_addr;
            self.last_seen_sync_addr = self.election.sync_addr;
            ElectionTransitions {
                master_addr_changed,
                sync_addr_changed,
            }
        }
    }

    /// Result of [`AwdlState::note_election_transitions`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ElectionTransitions {
        pub master_addr_changed: bool,
        pub sync_addr_changed: bool,
    }

    /// A pending change to the system IPv6 neighbor table, surfaced by
    /// [`NeighborTable`]. The runtime applies these via `os::rtnl::neighbor_*`
    /// (RTM_NEWNEIGH / RTM_DELNEIGH). FILIN_NEIGHBOR_TABLE.md.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NeighborChange {
        /// Add/refresh `{ipv6, mac}` on the host interface.
        Add([u8; 6], [u8; 16]),
        /// Remove the entry for this ipv6 (peer evicted).
        Remove([u8; 6], [u8; 16]),
        /// No change needed (mapping already present / absent).
        NoChange,
    }

    /// Tracks which peer MAC→IPv6 mappings filin has installed in the system
    /// neighbor table, so it only issues netlink calls on actual changes (not
    /// on every received frame). FILIN_NEIGHBOR_TABLE.md. AWDL does NOT use
    /// NDP; instead, on each received action frame the peer's link-local IPv6
    /// is derived from its source MAC (RFC 4291 modified EUI-64) and a static
    /// neighbor entry created. Without this, all unicast awdl0 connections
    /// fail with "No route to host" and AirDrop's /Discover never completes.
    #[derive(Debug, Default)]
    pub struct NeighborTable {
        known: std::collections::BTreeSet<[u8; 6]>,
    }

    impl NeighborTable {
        pub fn new() -> Self {
            Self::default()
        }

        /// Record that `mac` was just seen. Returns [`NeighborChange::Add`]
        /// the first time a MAC is sighted, [`NeighborChange::NoChange`] on
        /// subsequent sightings (no table churn).
        pub fn note_peer_seen(&mut self, mac: &[u8; 6]) -> NeighborChange {
            if self.known.contains(mac) {
                return NeighborChange::NoChange;
            }
            self.known.insert(*mac);
            NeighborChange::Add(*mac, crate::awdl::link_local_ipv6(*mac))
        }

        /// Evict every known MAC that is NOT in `live_macs` (the peer table
        /// after a clean pass). Returns one [`NeighborChange::Remove`] per
        /// evicted MAC.
        pub fn evict_stale(&mut self, live_macs: &[[u8; 6]]) -> Vec<NeighborChange> {
            let live: std::collections::BTreeSet<[u8; 6]> = live_macs.iter().copied().collect();
            let to_remove: Vec<[u8; 6]> = self.known.difference(&live).copied().collect();
            let mut out = Vec::with_capacity(to_remove.len());
            for mac in to_remove {
                self.known.remove(&mac);
                out.push(NeighborChange::Remove(mac, crate::awdl::link_local_ipv6(mac)));
            }
            out
        }
    }

    /// owl `awdl_handle_election_params_tlv` (owl/src/rx.c:111-131).
    fn apply_election_v1(peer: &mut Peer, value: &[u8]) {
        if let Ok(e) = awdl::parse_election_parameters(value) {
            peer.election.height = u32::from(e.height);
            peer.election.master_addr = e.master_addr;
            peer.election.master_metric = e.master_metric;
            peer.election.self_metric = e.self_metric;
        }
    }

    /// owl `awdl_handle_election_params_v2_tlv` (owl/src/rx.c:133-149).
    fn apply_election_v2(peer: &mut Peer, value: &[u8]) {
        if let Ok(e) = awdl::parse_election_parameters_v2(value) {
            peer.election.master_addr = e.master_addr;
            peer.election.sync_addr = e.sync_addr;
            peer.election.master_counter = e.master_counter;
            peer.election.height = e.distance_to_master;
            peer.election.master_metric = e.master_metric;
            peer.election.self_metric = e.self_metric;
            peer.election.self_counter = e.self_counter;
            peer.supports_v2 = true;
        }
    }

    /// Format a byte slice as a hex string for debug logging.
    fn hex_encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    /// owl `awdl_handle_chanseq_tlv` (owl/src/rx.c:60-109): store the peer's
    /// 16-slot channel sequence (as raw `(chan_num, opclass)` bytes).
    fn apply_chanseq(peer: &mut Peer, value: &[u8]) {
        if let Ok(seq) = awdl::parse_channel_sequence(value) {
            let decoded: Vec<u8> = seq.channels.iter().map(|&c| c as u8).collect();
            tracing::debug!(
                raw = %hex_encode(value),
                encoding = seq.encoding,
                decoded = ?decoded,
                peer = ?peer.addr,
                "chanseq TLV decoded"
            );
            for (slot, ch) in seq.channels.iter().take(CHANSEQ_LEN).enumerate() {
                peer.sequence[slot] = ch.to_le_bytes();
            }
        } else {
            tracing::debug!(
                raw = %hex_encode(value),
                peer = ?peer.addr,
                "chanseq TLV parse failed — peer sequence unchanged"
            );
        }
    }

    /// owl `awdl_handle_arpa_tlv` (owl/src/rx.c:151-158).
    fn apply_arpa(peer: &mut Peer, value: &[u8]) {
        if value.len() >= 2 {
            let name_len = usize::from(value[1]);
            let end = 2 + name_len;
            if end <= value.len() {
                peer.name = String::from_utf8_lossy(&value[2..end]).into_owned();
            }
        }
    }

    /// owl `awdl_handle_version_tlv` (owl/src/rx.c:196-206).
    fn apply_version(peer: &mut Peer, value: &[u8]) {
        if let (Some(v), Some(d)) = (value.first(), value.get(1)) {
            peer.version = *v;
            peer.devclass = *d;
        }
    }

    /// opclass byte for an AWDL social channel (owl `CHAN_OPCLASS_*`,
    /// owl/src/channel.h:51-53).
    pub fn chan_opclass(channel: u16) -> u8 {
        match channel {
            6 => 0x51,
            44 | 149 => 0x80,
            _ => 0x80,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn mif() -> awdl::ActionSubtype {
            awdl::ActionSubtype::Mif
        }

        fn version_tlv(version: u8, devclass: u8) -> awdl::Tlv<'static> {
            awdl::Tlv {
                kind: 21,
                value: Box::leak(vec![version, devclass].into_boxed_slice()),
            }
        }

        fn election_v2_tlv(master_counter: u32, metric: u32) -> awdl::Tlv<'static> {
            let mut value = vec![0u8; 40];
            value[0..6].copy_from_slice(&[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]); // master
            value[6..12].copy_from_slice(&[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]); // sync (self)
            value[12..16].copy_from_slice(&master_counter.to_le_bytes());
            value[16..20].copy_from_slice(&0u32.to_le_bytes()); // distance
            value[20..24].copy_from_slice(&metric.to_le_bytes());
            value[24..28].copy_from_slice(&metric.to_le_bytes());
            value[36..40].copy_from_slice(&0u32.to_le_bytes());
            awdl::Tlv { kind: 24, value: value.leak() }
        }

        /// Like [`election_v2_tlv`] but with a configurable peer address, so
        /// two distinct masters can be made to compete in one state.
        fn election_v2_tlv_for(
            addr: [u8; 6],
            master_counter: u32,
            metric: u32,
        ) -> awdl::Tlv<'static> {
            let mut value = vec![0u8; 40];
            value[0..6].copy_from_slice(&addr); // master
            value[6..12].copy_from_slice(&addr); // sync (self)
            value[12..16].copy_from_slice(&master_counter.to_le_bytes());
            value[16..20].copy_from_slice(&0u32.to_le_bytes()); // distance
            value[20..24].copy_from_slice(&metric.to_le_bytes());
            value[24..28].copy_from_slice(&metric.to_le_bytes());
            value[36..40].copy_from_slice(&0u32.to_le_bytes());
            awdl::Tlv { kind: 24, value: value.leak() }
        }

        /// Build a chanseq TLV value (OPCLASS encoding) filled with `chan`.
        fn chanseq_value_all(chan: u8) -> Vec<u8> {
            let mut v = vec![15, awdl::CHAN_ENC_OPCLASS, 0, 3];
            v.extend_from_slice(&0xffffu16.to_le_bytes());
            for _ in 0..16 {
                v.extend_from_slice(&[chan, 0x80]);
            }
            v
        }

        #[test]
        fn state_inits_self_master_and_static_chanseq() {
            let state = AwdlState::new([0x02, 1, 2, 3, 4, 5], "filin".into(), 44, 0);
            assert_eq!(state.election.master_addr, [0x02, 1, 2, 3, 4, 5]);
            assert_eq!(state.election.master_metric, ElectionState::new([0; 6]).master_metric);
            assert_eq!(state.channel_sequence[0], [44, 0x80]);
            assert_eq!(state.version, AWDL_VERSION);
            assert_eq!(state.dev_class, AWDL_DEVCLASS_MACOS);
        }

        #[test]
        fn force_master_self_sequence_stays_on_single_anchor() {
            // force-master keeps the static single-anchor sequence (a full
            // 6/44/149 rotation was tried and made AirDrop worse — staying
            // parked on the anchor keeps a stronger presence where peers meet).
            let mut state = AwdlState::new([0x02, 1, 2, 3, 4, 5], "filin".into(), 44, 0);
            state.election.force_self_master(crate::election::DEFAULT_FORCE_MASTER_METRIC);
            // Trigger sync_channel_sequence (runs inside clean_peers).
            state.clean_peers(0);
            assert!(
                state.channel_sequence.iter().all(|s| s[0] == 44),
                "force-master seq must stay on the single -c anchor (44)"
            );
        }

        #[test]
        fn apply_action_registers_peer_and_adopts_master() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 1_000_000);
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            // peer advertises a high master counter; needs version+devclass+mif
            // to become valid before election will adopt it.
            let tlvs = vec![
                election_v2_tlv(50, 200),
                version_tlv(0x34, 1),
            ];
            state.apply_action(peer_addr, mif(), &tlvs, 1_100_000);

            let peer = state.peers.get(&peer_addr).unwrap();
            assert!(peer.is_valid);
            assert!(peer.sent_mif);
            assert_eq!(peer.version, 0x34);
            // election should have adopted the peer as master
            assert_eq!(state.election.sync_addr, peer_addr);
            assert_eq!(state.election.master_counter, 50);
            assert_eq!(state.election.height, 1);
        }

        #[test]
        fn apply_action_ignores_own_self_addressed_frame() {
            // In monitor mode the card hears its own injected frames. owl drops
            // frames sourced from self (rx.c); filin must too, or it registers
            // itself as a peer and elects itself master — observed live as
            // violent master flapping (own MAC in last_master_macs) that
            // destroys sync with real peers. A self-sourced frame, even one
            // advertising a strong master, must NOT register a peer or move the
            // election off self-master.
            let self_addr = [0x02, 0, 0, 0, 0, 1];
            let mut state = AwdlState::new(self_addr, "filin".into(), 44, 1_000_000);
            let tlvs = vec![election_v2_tlv(50, 200), version_tlv(0x34, 1)];

            state.apply_action(self_addr, mif(), &tlvs, 1_100_000);

            // No self-peer registered, and the node stays its own master.
            assert!(state.peers.get(&self_addr).is_none());
            assert_eq!(state.election.sync_addr, self_addr);
            assert_eq!(state.election.master_counter, 0);
        }

        #[test]
        fn sync_only_adopted_from_elected_master() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 1_000_000);
            let master_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            let other_addr = [0x02, 0, 0, 0, 0, 9];

            // make master_addr the elected master first
            state.apply_action(
                master_addr,
                mif(),
                &[election_v2_tlv(50, 200), version_tlv(0x34, 1)],
                1_100_000,
            );
            assert!(state.election.is_sync_master(&master_addr));
            let aw_counter_before = state.sync.aw_counter;

            // a sync TLV from a non-master must NOT move our clock
            let mut sync_val = vec![0u8; 31];
            sync_val[1..3].copy_from_slice(&10u16.to_le_bytes());
            sync_val[29..31].copy_from_slice(&16u16.to_le_bytes());
            let non_master_tlv = awdl::Tlv { kind: 4, value: sync_val.leak() };
            state.apply_action(other_addr, mif(), std::slice::from_ref(&non_master_tlv), 1_200_000);
            assert_eq!(state.sync.aw_counter, aw_counter_before, "non-master must not move sync");
        }

        #[test]
        fn clean_peers_evicts_stale_and_reruns_election() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.apply_action(
                peer_addr,
                mif(),
                &[election_v2_tlv(50, 200), version_tlv(0x34, 1)],
                1_000,
            );
            assert_eq!(state.election.sync_addr, peer_addr);

            // after the timeout with no further updates the peer is gone and we
            // fall back to being our own master
            state.clean_peers(1_000 + peers::PEER_TIMEOUT_US + 1);
            assert!(state.peers.get(&peer_addr).is_none());
            assert_eq!(state.election.sync_addr, state.election.self_addr);
        }

        #[test]
        fn adopts_master_channel_sequence_on_election() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            // build a chanseq TLV value with mixed channels
            let mut chanseq_val = vec![15, 3, 0, 3];
            chanseq_val.extend_from_slice(&0xffffu16.to_le_bytes());
            for i in 0..16 {
                let ch = match i {
                    0..=7 => 149u16,
                    _ => 6u16,
                };
                chanseq_val.extend_from_slice(&ch.to_le_bytes());
            }
            let tlvs = vec![
                election_v2_tlv(50, 200),
                version_tlv(0x34, 1),
                awdl::Tlv { kind: 18, value: &chanseq_val },
            ];
            state.apply_action(peer_addr, mif(), &tlvs, 1_000);

            // filin should have adopted the master's sequence. The master
            // advertised 8x149 + 8x6; social coverage injected the missing
            // social channel 44. All three social channels are present.
            let chans: Vec<u8> =
                state.channel_sequence.iter().map(|s| s[0]).collect();
            assert!(chans.contains(&149), "149 from master");
            assert!(chans.contains(&6), "6 from master");
            assert!(chans.contains(&44), "44 injected by social coverage");
        }

        #[test]
        fn reverts_to_static_anchor_sequence_when_master_lost() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            // adopt a master with a non-44 sequence
            let mut chanseq_val = vec![15, 3, 0, 3];
            chanseq_val.extend_from_slice(&0xffffu16.to_le_bytes());
            for _ in 0..16 {
                chanseq_val.extend_from_slice(&149u16.to_le_bytes());
            }
            let tlvs = vec![
                election_v2_tlv(50, 200),
                version_tlv(0x34, 1),
                awdl::Tlv { kind: 18, value: &chanseq_val },
            ];
            state.apply_action(peer_addr, mif(), &tlvs, 1_000);
            // 149 present after adoption (social coverage injects 6/44 but
            // preserves at least one 149).
            assert!(state.channel_sequence.iter().any(|s| s[0] == 149));

            // master times out → revert to anchor (44)
            state.clean_peers(1_000 + peers::PEER_TIMEOUT_US + 1);
            assert_eq!(state.channel_sequence[0][0], 44);
        }

        #[test]
        fn adopts_legacy_chanseq_to_real_channels_not_flags_bytes() {
            // Regression for the live iPhone /Upload stall: a master that
            // advertises a LEGACY-encoded (encoding 1) channel sequence must
            // decode to REAL Wi-Fi channels, not the raw flags byte. filin
            // previously stored channel_sequence[slot][0] = flags (e.g.
            // 174/232/245), tuned the radio to invalid channels, and was
            // never co-channel with the iPhone for the sustained upload.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            // LEGACY: [flags=0xa4, chan_num]. Channels 44 then 149.
            let mut chanseq_val = vec![15, awdl::CHAN_ENC_LEGACY, 0, 3];
            chanseq_val.extend_from_slice(&0xffffu16.to_le_bytes());
            for i in 0..16 {
                let ch = if i < 8 { 44u8 } else { 149u8 };
                chanseq_val.extend_from_slice(&[0xa4, ch]);
            }
            let tlvs = vec![
                election_v2_tlv(50, 200),
                version_tlv(0x34, 1),
                awdl::Tlv { kind: 18, value: &chanseq_val },
            ];
            state.apply_action(peer_addr, mif(), &tlvs, 1_000);
            // Real channels — NOT the 0xa4 flags byte (164) — and all three
            // social channels present after social-coverage injection.
            for slot in &state.channel_sequence {
                assert_ne!(slot[0], 0xa4, "no flags bytes in channel sequence");
            }
            let chans: Vec<u8> =
                state.channel_sequence.iter().map(|s| s[0]).collect();
            assert!(chans.contains(&44));
            assert!(chans.contains(&149));
            assert!(chans.contains(&6));
        }

        #[test]
        fn master_hysteresis_debounces_challenger_switch() {
            // Bug B: on a contended link two Apple devices compete; filin
            // must NOT re-adopt on every frame. A single challenger frame
            // holds; a sustained win over the debounce window commits.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_a = [0x02, 0xaa, 0, 0, 0, 1];
            let peer_b = [0x02, 0xbb, 0, 0, 0, 2];

            // Adopt master A (counter 50, chan 149) — first adoption is
            // immediate (self → real master is not a flip-flop).
            let chan_a = chanseq_value_all(149);
            state.apply_action(
                peer_a,
                mif(),
                &[
                    election_v2_tlv_for(peer_a, 50, 200),
                    version_tlv(0x34, 1),
                    awdl::Tlv { kind: 18, value: &chan_a },
                ],
                1_000,
            );
            assert_eq!(state.committed_master, peer_a);
            // A's chanseq (16x149 → social coverage injects 6/44); 149
            // remains dominant (14 slots).
            assert!(
                state.channel_sequence.iter().filter(|s| s[0] == 149).count() >= 14,
                "A's chanseq must be in place"
            );

            // Master B (higher counter 60, chan 6) wins one frame — but it
            // is a challenger, so filin holds A.
            let chan_b = chanseq_value_all(6);
            state.apply_action(
                peer_b,
                mif(),
                &[
                    election_v2_tlv_for(peer_b, 60, 200),
                    version_tlv(0x34, 1),
                    awdl::Tlv { kind: 18, value: &chan_b },
                ],
                2_000,
            );
            assert_eq!(state.committed_master, peer_a, "single frame must not flip");
            assert!(
                state.channel_sequence.iter().filter(|s| s[0] == 149).count() >= 14,
                "chanseq must stay A's"
            );

            // B wins again after the debounce window → commit to B.
            let now = 2_000 + crate::schedule::MASTER_DEBOUNCE_US;
            state.apply_action(
                peer_b,
                mif(),
                &[
                    election_v2_tlv_for(peer_b, 60, 200),
                    version_tlv(0x34, 1),
                    awdl::Tlv { kind: 18, value: &chan_b },
                ],
                now,
            );
            assert_eq!(state.committed_master, peer_b, "sustained win must commit");
            // B's chanseq (16x6 → social coverage injects 44/149); 6
            // becomes dominant (14 slots).
            assert!(
                state.channel_sequence.iter().filter(|s| s[0] == 6).count() >= 14,
                "B's chanseq must be adopted"
            );
        }

        #[test]
        fn transfer_pin_holds_channel_and_suppresses_readoption() {
            // Bug C: while a unicast transfer is active, filin PINs the radio
            // to the peer's RX channel and suppresses hopping + master
            // re-adoption so the bulk upload is not cut off mid-stream.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let peer_a = [0x02, 0xaa, 0, 0, 0, 1];
            let peer_b = [0x02, 0xbb, 0, 0, 0, 2];

            // Adopt master A (chanseq all 149 → social coverage injects
            // 6/44 into early slots; slot 0 becomes 6, but the pin test
            // only cares that the pin overrides the slot channel).
            let chan_a = chanseq_value_all(149);
            state.apply_action(
                peer_a,
                mif(),
                &[
                    election_v2_tlv_for(peer_a, 50, 200),
                    version_tlv(0x34, 1),
                    awdl::Tlv { kind: 18, value: &chan_a },
                ],
                1_000,
            );
            // Slot 0 after social coverage is 6 (injected). desired_channel
            // returns the slot channel when no pin is active.
            let slot0_ch = state.desired_channel(1_000);
            assert!(slot0_ch != 0);

            // Arm the sticky pin: we just RX'd unicast from A on ch44.
            state.note_unicast_rx(peer_a, 44, 1_000);
            // desired_channel now returns the PINNED 44, not the slot
            // channel, so the runtime does not hop the radio away mid-upload.
            assert_eq!(state.desired_channel(1_000), 44);

            // Master B (higher counter, chan 6) tries to take over — the pin
            // suppresses re-adoption, so committed stays at A.
            let chan_b = chanseq_value_all(6);
            state.apply_action(
                peer_b,
                mif(),
                &[
                    election_v2_tlv_for(peer_b, 60, 200),
                    version_tlv(0x34, 1),
                    awdl::Tlv { kind: 18, value: &chan_b },
                ],
                1_100,
            );
            assert_eq!(state.committed_master, peer_a, "pin suppresses readoption");

            // After the idle timeout the pin releases; desired_channel
            // reverts to the slot channel and re-adoption can proceed.
            let after = 1_100 + crate::schedule::TRANSFER_IDLE_TIMEOUT_US + 1;
            assert_ne!(state.desired_channel(after), 44); // pin released
        }

        #[test]
        fn update_channel_detects_needed_switch() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            // current_channel starts at 0; desired is 44 (slot 0)
            let switch = state.update_channel(0);
            assert_eq!(switch, Some(44));
            assert_eq!(state.current_channel, 44);

            // calling again: no switch needed
            assert_eq!(state.update_channel(0), None);
        }

        #[test]
        fn desired_channel_reflects_sequence_slot() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            // put a null slot at position 1
            state.channel_sequence[1] = [0, 0];

            // slot 0 (t=0): channel 44
            assert_eq!(state.desired_channel(0), 44);
            // advance to EAW slot 1: 64 TU later = 65536 us
            assert_eq!(state.desired_channel(65536), 0);
        }

        // --- FILIN_SYNC_QUALITY.md pivot: single-channel park mode ---

        #[test]
        fn park_channel_picks_top_master_anchor_from_peers() {
            // Two peers report the SAME top master_addr; their slot-0 channels
            // are 149 and 149 (one says 44). The dominant TX-capable slot-0
            // across peers reporting the elected master_addr is 149 → park
            // there. (TX-capable channels only; No-IR avoidance is covered by
            // park_channel_never_parks_on_a_no_ir_channel.)
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let top = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.election.master_addr = top;

            let p1 = [0x02, 0xc1, 0, 0, 0, 1];
            let p2 = [0x02, 0xc2, 0, 0, 0, 2];
            state.peers.touch(p1, 0);
            state.peers.touch(p2, 0);
            {
                let peer = state.peers.get_mut(&p1).unwrap();
                peer.election.master_addr = top;
                peer.sequence[0] = [149, 0x80];
            }
            {
                let peer = state.peers.get_mut(&p2).unwrap();
                peer.election.master_addr = top;
                peer.sequence[0] = [149, 0x80];
            }
            assert_eq!(state.park_channel(), 149);
        }

        #[test]
        fn park_channel_falls_back_to_anchor_when_no_peers() {
            // No peers heard yet → park on the configured anchor (44).
            let state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            assert_eq!(state.park_channel(), 44);
        }

        #[test]
        fn park_channel_never_parks_on_a_no_ir_channel() {
            // The top master anchors on ch52 (No-IR/DFS). Parking there would
            // strand filin on a channel the carl9170 can't TX on — it could
            // neither announce nor answer /Discover|/Ask (observed live: parked
            // on 52, every transfer failed). park_channel must skip No-IR and
            // fall back to a TX-capable channel (the configured anchor 44).
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let top = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.election.master_addr = top;
            let p1 = [0x02, 0xc1, 0, 0, 0, 1];
            state.peers.touch(p1, 0);
            {
                let peer = state.peers.get_mut(&p1).unwrap();
                peer.election.master_addr = top;
                peer.sequence[0] = [52, 0x80]; // No-IR anchor
            }
            let ch = state.park_channel();
            assert!(!crate::channel::is_no_ir_channel(ch), "must not park on No-IR ch{ch}");
            assert_eq!(ch, 44); // falls back to the TX-capable configured anchor
        }

        #[test]
        fn desired_channel_in_park_mode_ignores_eaw_slot() {
            // In park mode desired_channel returns the parked channel for
            // EVERY slot — no hopping. Even a null slot in the sequence must
            // not produce channel 0.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            state.channel_sequence[1] = [0, 0]; // would be 0 in hop mode
            state.enable_park(52);
            assert_eq!(state.desired_channel(0), 52);
            assert_eq!(state.desired_channel(65536), 52, "park ignores slot 1 null");
        }

        #[test]
        fn update_channel_in_park_mode_only_switches_when_park_channel_changes() {
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            state.enable_park(52);
            // First call: switch to 52.
            assert_eq!(state.update_channel(0), Some(52));
            assert_eq!(state.current_channel, 52);
            // Subsequent calls: no switch (parked).
            assert_eq!(state.update_channel(65536), None);
            assert_eq!(state.update_channel(131072), None);
            // Park channel changes (top master anchor moved) → switch once.
            state.enable_park(44);
            assert_eq!(state.update_channel(131072), Some(44));
            assert_eq!(state.current_channel, 44);
        }

        // --- FILIN_SYNC_QUALITY.md regression: peers retained + channel
        //     sequence re-adopted through normal cluster churn. ---

        fn peer_with_cluster(addr: [u8; 6], top: [u8; 6], counter: u32) -> awdl::Tlv<'static> {
            let mut v = vec![0u8; 40];
            v[0..6].copy_from_slice(&top);
            v[6..12].copy_from_slice(&addr);
            v[12..16].copy_from_slice(&counter.to_le_bytes());
            v[16..20].copy_from_slice(&0u32.to_le_bytes());
            v[20..24].copy_from_slice(&200u32.to_le_bytes());
            v[24..28].copy_from_slice(&200u32.to_le_bytes());
            v[36..40].copy_from_slice(&0u32.to_le_bytes());
            awdl::Tlv { kind: 24, value: v.leak() }
        }

        fn chanseq_tlv(slot0: u8) -> awdl::Tlv<'static> {
            let mut v = vec![15, awdl::CHAN_ENC_OPCLASS, 0, 3];
            v.extend_from_slice(&0xffffu16.to_le_bytes());
            for _ in 0..16 {
                v.extend_from_slice(&[slot0, 0x80]);
            }
            awdl::Tlv { kind: 18, value: v.leak() }
        }

        #[test]
        fn normal_churn_retains_peers_and_readopts_channel_sequence() {
            // Regression: multiple peers from the same cluster arrive in
            // sequence. ALL must be retained in the peer table, and the
            // channel sequence must be re-adopted from each new peer so filin
            // stays co-channel ("channel surfing"). This is what keeps
            // peer_count > 0 through master churn in hopping mode. The old
            // re-parent hysteresis froze the sequence after the first peer,
            // breaking the surfing — peers went stale → evicted → peer_count=0.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            let top = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            let peer_a = [0x02, 0xc1, 0, 0, 0, 1];
            let peer_b = [0x02, 0xc2, 0, 0, 0, 2];

            state.apply_action(
                peer_a,
                mif(),
                &[
                    peer_with_cluster(peer_a, top, 50),
                    version_tlv(0x34, 1),
                    chanseq_tlv(149),
                ],
                1_000,
            );
            assert_eq!(state.peers.len(), 1, "peer A added");
            assert!(state.channel_sequence.iter().any(|s| s[0] == 149));

            // Peer B arrives (same cluster top, higher counter, slot0 = 6).
            // filin MUST re-adopt B's channel sequence (channel surfing) and
            // B MUST be in the peer table. In the live environment frames
            // arrive continuously (~160ms PSF interval); simulate by sending
            // B twice past the Bug B debounce window (first frame starts the
            // pending clock, second frame past debounce commits).
            let b_t1 = 2_000;
            let b_t2 = b_t1 + crate::schedule::MASTER_DEBOUNCE_US + 1;
            for t in [b_t1, b_t2] {
                state.apply_action(
                    peer_b,
                    mif(),
                    &[
                        peer_with_cluster(peer_b, top, 60),
                        version_tlv(0x34, 1),
                        chanseq_tlv(6),
                    ],
                    t,
                );
            }
            assert_eq!(state.peers.len(), 2, "both peers A and B retained");
            // Channel sequence re-adopted from B: 6 must DOMINATE (>=14 slots
            // after social coverage), not 149. If the sequence were frozen at
            // A's, 149 would dominate instead — that's the regression.
            let count_6 = state.channel_sequence.iter().filter(|s| s[0] == 6).count();
            let count_149 = state.channel_sequence.iter().filter(|s| s[0] == 149).count();
            assert!(
                count_6 > count_149,
                "6 must dominate after re-adopting B (6:{} vs 149:{}), \
                 not frozen at A's 149-dominant sequence",
                count_6,
                count_149
            );
            assert_ne!(
                state.election.sync_addr, state.self_addr,
                "synced to an external parent"
            );
        }

        // --- FILIN_SYNC_QUALITY.md: master_addr vs sync_addr transitions ---

        #[test]
        fn election_transitions_detect_sync_addr_reparent_without_master_addr_change() {
            // The core churn diagnosis: sync_addr (parent) flips while
            // master_addr (cluster top) stays stable. The transition detector
            // must report sync_addr_changed but NOT master_addr_changed.
            let mut state = AwdlState::new([0x02, 0, 0, 0, 0, 1], "filin".into(), 44, 0);
            // First call seeds the baseline; no transition reported.
            let t0 = state.note_election_transitions();
            assert!(!t0.master_addr_changed && !t0.sync_addr_changed);

            // Adopt the cluster top master via peer A.
            state.election.master_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.election.sync_addr = [0x02, 0xcc, 0, 0, 0, 1]; // parent A
            let t1 = state.note_election_transitions();
            assert!(t1.master_addr_changed, "top master changed");
            assert!(t1.sync_addr_changed, "parent changed");

            // Re-parent to peer B (same top master): sync_addr flips,
            // master_addr stable.
            state.election.sync_addr = [0x02, 0xdd, 0, 0, 0, 2]; // parent B
            let t2 = state.note_election_transitions();
            assert!(!t2.master_addr_changed, "top master stable across re-parent");
            assert!(t2.sync_addr_changed, "parent changed on re-parent");
        }

        // --- FILIN_NEIGHBOR_TABLE.md: neighbor table change decisions ---

        #[test]
        fn neighbor_change_adds_on_first_sight_of_a_mac() {
            let mut table = NeighborTable::new();
            let mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            // First sight → Add.
            assert_eq!(
                table.note_peer_seen(&mac),
                NeighborChange::Add(mac, awdl::link_local_ipv6(mac))
            );
            // Second sight → NoChange (already in the table; don't churn).
            assert_eq!(table.note_peer_seen(&mac), NeighborChange::NoChange);
        }

        #[test]
        fn neighbor_change_evicts_removed_macs() {
            let mut table = NeighborTable::new();
            let mac_a = [0x02, 0xaa, 0, 0, 0, 1];
            let mac_b = [0x02, 0xbb, 0, 0, 0, 2];
            table.note_peer_seen(&mac_a);
            table.note_peer_seen(&mac_b);

            // mac_a is no longer live (only mac_b in the live set) → mac_a
            // is evicted; mac_b stays.
            let removed = table.evict_stale(&[mac_b]);
            assert_eq!(
                removed,
                vec![NeighborChange::Remove(mac_a, awdl::link_local_ipv6(mac_a))]
            );
            // Evicting when the only known MAC (mac_b) is still live → nothing.
            let removed = table.evict_stale(&[mac_b]);
            assert!(removed.is_empty());
            // mac_b still in the table.
            assert_eq!(table.note_peer_seen(&mac_b), NeighborChange::NoChange);
        }

        #[test]
        fn neighbor_change_evicts_only_peers_no_longer_seen() {
            let mut table = NeighborTable::new();
            let mac_a = [0x02, 0xaa, 0, 0, 0, 1];
            let mac_b = [0x02, 0xbb, 0, 0, 0, 2];
            let mac_c = [0x02, 0xcc, 0, 0, 0, 3];
            table.note_peer_seen(&mac_a);
            table.note_peer_seen(&mac_b);
            table.note_peer_seen(&mac_c);

            // mac_b is still alive (in live set) → NOT evicted.
            // mac_a and mac_c evicted.
            let removed = table.evict_stale(&[mac_b]);
            let removed_macs: Vec<[u8; 6]> = removed
                .iter()
                .filter_map(|c| match c {
                    NeighborChange::Remove(mac, _) => Some(*mac),
                    _ => None,
                })
                .collect();
            assert!(removed_macs.contains(&mac_a));
            assert!(removed_macs.contains(&mac_c));
            assert!(!removed_macs.contains(&mac_b));
        }
    }
}

pub mod runtime {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    use crate::{
        awdl, channel, ieee80211, iface::Config, os, pcap, radiotap, rx, sync::TsftBridge,
    };

    #[derive(Debug, PartialEq, Eq)]
    pub enum Error {
        Tap,
        Netdev,
        Packet,
        Channel,
        Poll,
        Io,
        Pcap,
        /// The monitor adapter is permanently unusable (e.g. its driver does
        /// not support monitor mode). Carries a human-readable explanation.
        /// Distinct from the transient errors above so the supervisor loop can
        /// stop retrying and exit instead of spinning forever on a card that
        /// will never work.
        UnsupportedAdapter(String),
    }

    /// A capability assessment of a candidate monitor interface, produced by
    /// [`probe_adapter`]. Kept separate from the probing I/O so the
    /// message-formatting logic ([`AdapterReport::summary`]) is pure and
    /// unit-testable.
    #[derive(Debug, PartialEq, Eq)]
    pub struct AdapterReport {
        pub iface: String,
        /// Whether the interface name currently resolves (is the card present?).
        pub exists: bool,
        /// The driver's supported iftypes, or `None` if the capability probe
        /// itself failed (so we genuinely don't know).
        pub iftypes: Option<Vec<u32>>,
    }

    impl AdapterReport {
        /// `Some(true)`/`Some(false)` when we positively know; `None` if the
        /// probe was inconclusive.
        pub fn supports_monitor(&self) -> Option<bool> {
            self.iftypes.as_ref().map(|t| t.contains(&6))
        }

        /// True only when we are CERTAIN the adapter can't work (missing, or
        /// the driver advertises no monitor mode). Inconclusive probes return
        /// `false` so filin still attempts the normal bring-up.
        pub fn is_unusable(&self) -> bool {
            !self.exists || self.supports_monitor() == Some(false)
        }

        /// A multi-line, operator-facing explanation of the verdict.
        pub fn summary(&self) -> String {
            use crate::os::nl80211::iftype_name;
            if !self.exists {
                return format!(
                    "interface '{}' not found — is the adapter plugged in? (check `ip link`)",
                    self.iface
                );
            }
            match &self.iftypes {
                None => format!(
                    "'{}' exists but its capabilities could not be probed (nl80211 GET_WIPHY \
                     failed); proceeding without a verdict",
                    self.iface
                ),
                Some(types) => {
                    let modes = types
                        .iter()
                        .map(|t| iftype_name(*t).into_owned())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if types.contains(&6) {
                        format!(
                            "'{}' supports monitor mode (driver modes: {modes}).\n\
                             NOTE: monitor support does NOT guarantee data-frame INJECTION. Some \
                             adapters (notably Realtek rtw88 / 8822bu) accept monitor mode and \
                             inject action frames but silently drop DATA frames, so AirDrop \
                             payloads never transmit. The known-good injector is Atheros carl9170. \
                             See the README 'Hardware' section.",
                            self.iface
                        )
                    } else {
                        format!(
                            "'{}' does NOT support monitor mode — it cannot be used as filin's \
                             radio. Its driver advertises only: {modes}.\n\
                             Use a monitor- and injection-capable adapter; the known-good one is \
                             Atheros carl9170. See the README 'Hardware' section.",
                            self.iface
                        )
                    }
                }
            }
        }
    }

    /// Probe a candidate monitor interface's capabilities (existence + the
    /// modes its driver supports). Read-only; safe to call without root.
    pub fn probe_adapter(iface: &str) -> AdapterReport {
        let exists = {
            let cs = std::ffi::CString::new(iface).ok();
            // SAFETY: cs (when Some) is a valid NUL-terminated C string.
            cs.map(|c| unsafe { libc::if_nametoindex(c.as_ptr()) } != 0)
                .unwrap_or(false)
        };
        let iftypes = if exists {
            os::nl80211::supported_iftypes(iface).ok()
        } else {
            None
        };
        AdapterReport {
            iface: iface.to_string(),
            exists,
            iftypes,
        }
    }

    #[cfg(test)]
    mod adapter_tests {
        use super::{AdapterReport, Error};

        fn report(exists: bool, iftypes: Option<Vec<u32>>) -> AdapterReport {
            AdapterReport {
                iface: "wlxtest".into(),
                exists,
                iftypes,
            }
        }

        #[test]
        fn missing_interface_is_unusable_and_says_so() {
            let r = report(false, None);
            assert!(r.is_unusable());
            assert_eq!(r.supports_monitor(), None);
            let s = r.summary();
            assert!(s.contains("not found"), "summary: {s}");
            assert!(s.contains("wlxtest"));
        }

        #[test]
        fn no_monitor_mode_is_unusable_and_lists_modes() {
            let r = report(true, Some(vec![2, 3])); // managed, ap
            assert_eq!(r.supports_monitor(), Some(false));
            assert!(r.is_unusable());
            let s = r.summary();
            assert!(s.contains("does NOT support monitor mode"), "summary: {s}");
            assert!(s.contains("managed"));
            assert!(s.contains("carl9170"));
        }

        #[test]
        fn monitor_capable_is_usable_but_warns_about_injection() {
            let r = report(true, Some(vec![2, 6])); // managed, monitor
            assert_eq!(r.supports_monitor(), Some(true));
            assert!(!r.is_unusable());
            let s = r.summary();
            assert!(s.contains("supports monitor mode"), "summary: {s}");
            // The rtw88 "monitor OK but drops DATA frames" caveat must be loud.
            assert!(s.contains("INJECTION"));
            assert!(s.contains("rtw88"));
        }

        #[test]
        fn inconclusive_probe_is_not_treated_as_unusable() {
            let r = report(true, None);
            assert_eq!(r.supports_monitor(), None);
            assert!(!r.is_unusable(), "must still attempt bring-up when unsure");
            assert!(r.summary().contains("could not be probed"));
        }

        #[test]
        fn unsupported_adapter_error_carries_message() {
            // The fatal variant must round-trip the operator-facing text.
            let e = Error::UnsupportedAdapter("no monitor mode".into());
            assert_eq!(e, Error::UnsupportedAdapter("no monitor mode".into()));
        }
    }

    pub struct Links {
        pub tap: File,
        pub packet: os::packet::PacketSocket,
        self_addr: [u8; 6],
        anchor_channel: u16,
        monitor_iface: String,
        host_iface: String,
        pcap: Option<pcap::Writer<File>>,
    }

    /// Best-effort: tell NetworkManager to stop managing the monitor interface
    /// so it can't fight filin's monitor-mode config or try to (re)connect it
    /// in managed mode. After a USB unplug/replug the re-enumerated device
    /// commonly reverts to NM-managed. Idempotent, and silently ignored if
    /// nmcli / NetworkManager isn't present.
    fn release_from_network_manager(iface: &str) {
        match std::process::Command::new("nmcli")
            .args(["device", "set", iface, "managed", "no"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(s) if s.success() => {
                tracing::info!(iface, "released interface from NetworkManager (managed no)")
            }
            Ok(s) => tracing::debug!(
                iface,
                code = ?s.code(),
                "nmcli 'managed no' non-zero (NetworkManager may be absent)"
            ),
            Err(e) => {
                tracing::debug!(iface, ?e, "could not run nmcli (NetworkManager not installed?)")
            }
        }
    }

    pub fn open_links(config: &Config) -> Result<Links, Error> {
        let mon = config.monitor_iface.as_str();
        let tap = os::tun::open_tap(&config.host_iface).map_err(|e| {
            tracing::error!(
                host_iface = %config.host_iface,
                ?e,
                "could not create TAP interface '{}' — filin needs root (or CAP_NET_ADMIN)",
                config.host_iface
            );
            Error::Tap
        })?;
        tracing::debug!(host_iface = %config.host_iface, "opened TAP");
        let monitor_addr = os::netdev::get_hwaddr(mon).map_err(|e| {
            // The most common cause is a missing interface; turn that into an
            // actionable, adapter-aware message rather than a bare Netdev error.
            let report = probe_adapter(mon);
            tracing::error!(monitor_iface = %mon, ?e, "{}", report.summary());
            if report.is_unusable() {
                Error::UnsupportedAdapter(report.summary())
            } else {
                Error::Netdev
            }
        })?;
        tracing::debug!(monitor_iface = %mon, addr = ?monitor_addr, "read monitor hwaddr");
        os::netdev::set_hwaddr(&config.host_iface, monitor_addr).map_err(|_| Error::Netdev)?;
        os::netdev::set_mtu(&config.host_iface, 1450).map_err(|_| Error::Netdev)?;
        os::netdev::set_up(&config.host_iface).map_err(|_| Error::Netdev)?;
        tracing::debug!(host_iface = %config.host_iface, "host iface configured (addr/mtu/up)");
        // Configure the MONITOR interface so filin auto-recovers a card that was
        // bounced (USB reset / replug). After a bounce the adapter often
        // re-enumerates either in managed mode or administratively DOWN, which
        // breaks `set_channel` and the AF_PACKET socket.
        //
        // Unless `-N` (assume_monitor) is given, force monitor mode first
        // (down -> type monitor): this recovers a card that came back managed.
        // `-N` skips the mode change (assume it's configured externally), e.g.
        // when something else owns the radio's mode.
        if !config.assume_monitor {
            // Steal the iface from NetworkManager first, or NM will keep
            // re-managing it (managed mode) and fight our monitor config —
            // especially after a USB replug re-enumerates it as managed.
            release_from_network_manager(mon);
            let _ = os::netdev::set_down(mon); // best-effort
            os::nl80211::set_monitor(mon).map_err(|e| {
                // Failing to enter monitor mode is the signature of an
                // inadequate adapter. Probe its capabilities so the operator
                // gets a verdict instead of a bare errno; if the driver
                // genuinely has no monitor mode, make it fatal (no point
                // retrying a permanent condition every second).
                let report = probe_adapter(mon);
                tracing::error!(
                    monitor_iface = %mon,
                    ?e,
                    "failed to set monitor mode: {}",
                    report.summary()
                );
                if report.is_unusable() {
                    Error::UnsupportedAdapter(report.summary())
                } else {
                    Error::Netdev
                }
            })?;
            tracing::debug!(monitor_iface = %mon, "set monitor mode");
        }
        // Always bring it up before tuning (idempotent if already up); this
        // alone recovers a DOWN-but-still-monitor card even with `-N`.
        os::netdev::set_up(mon).map_err(|_| Error::Netdev)?;
        tracing::debug!(monitor_iface = %mon, "monitor iface up");
        let frequency_mhz =
            channel::channel_to_frequency_mhz(config.anchor_channel).ok_or(Error::Channel)?;
        os::nl80211::set_channel(mon, u32::from(frequency_mhz)).map_err(|e| {
            tracing::error!(
                monitor_iface = %mon,
                channel = config.anchor_channel,
                freq_mhz = frequency_mhz,
                ?e,
                "could not tune '{mon}' to channel {} ({} MHz) — the channel may be \
                 unsupported, No-IR, or DFS on this adapter; try a different -c (6/44/149)",
                config.anchor_channel,
                frequency_mhz
            );
            Error::Channel
        })?;
        let packet = os::packet::PacketSocket::open_bound(&config.monitor_iface)
            .map_err(|_| Error::Packet)?;
        let pcap = config
            .pcap_path
            .as_deref()
            .map(|path| {
                File::create(path)
                    .and_then(pcap::Writer::new)
                    .map_err(|_| Error::Pcap)
            })
            .transpose()?;

        Ok(Links {
            tap,
            packet,
            self_addr: monitor_addr,
            anchor_channel: config.anchor_channel,
            monitor_iface: config.monitor_iface.clone(),
            host_iface: config.host_iface.clone(),
            pcap,
        })
    }

    /// Poll `revents` bits that mean a fd is in an error / hang-up state rather
    /// than merely readable. `poll(2)` reports these even when only `POLLIN`
    /// was requested, and they are sticky until the error is consumed (e.g. via
    /// `recv`). The RX loop originally acted only on `POLLIN`, so a monitor
    /// socket that entered `POLLERR` (e.g. after a carl9170 USB reset / firmware
    /// -110) made `poll` return immediately every iteration with no `POLLIN`
    /// bit — a silent 100% CPU busy-spin with no RX and no progress.
    pub const POLL_ERR_BITS: libc::c_short = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;

    /// True when a pollfd's `revents` signals an error/hang-up (not just data).
    pub fn poll_revents_is_error(revents: libc::c_short) -> bool {
        revents & POLL_ERR_BITS != 0
    }

    /// Consecutive error-only poll iterations on the monitor socket tolerated
    /// before filin gives up and exits. A brief transient (rx-ring overflow, a
    /// single device-reset blip) clears well within this; a wedged radio
    /// (carl9170 firmware -110) never recovers on its own, so we exit loudly so
    /// the operator/supervisor can recover the card and restart, rather than
    /// linger as a zombie. With `POLL_ERROR_BACKOFF` this is ~1s of solid error.
    pub const POLL_ERROR_FATAL_THRESHOLD: u32 = 200;

    /// Backoff slept after each error-only poll iteration so a persistently
    /// failed socket cannot spin the CPU while we wait for it to recover or
    /// reach the fatal threshold.
    pub const POLL_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_millis(5);

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum PollErrorAction {
        /// Keep running — drain the error and retry (transient).
        Continue,
        /// Sustained failure — stop the RX loop so the daemon can be restarted.
        Fatal,
    }

    /// Escalation policy for a monitor-socket error: tolerate transients up to
    /// `threshold` consecutive error iterations, then declare it fatal.
    pub fn poll_error_action(consecutive_errors: u32, threshold: u32) -> PollErrorAction {
        if consecutive_errors >= threshold {
            PollErrorAction::Fatal
        } else {
            PollErrorAction::Continue
        }
    }

    pub fn run(
        mut links: Links,
        introspect: std::sync::Arc<crate::introspect::Introspection>,
        http_addr: Option<std::net::SocketAddr>,
        park: bool,
        disable_rssi_filter: bool,
        force_master: Option<u32>,
        tx_retransmits: u32,
    ) -> Result<(), Error> {
        if tx_retransmits > 0 {
            println!("tx-retransmits: re-injecting each unicast data frame {tx_retransmits}x extra (with Retry bit)");
        }
        let mut bridge = TsftBridge::default();
        let mut awdl_state = crate::state::AwdlState::new(
            links.self_addr,
            hostname(),
            links.anchor_channel,
            host_time_us(),
        );
        // --force-master: seed the election so filin wins and stays master,
        // forcing the cluster onto filin's social-channel sequence.
        if let Some(metric) = force_master {
            awdl_state.election.force_self_master(metric);
            println!("force-master enabled: advertising election metric {metric}");
        }
        let mut announce = AnnounceScheduler::new(host_time_us());
        let mut wlan_buf = vec![0u8; 4096];
        let mut tap_buf = vec![0u8; 2048];
        // owl keeps at most one pending unicast frame (`state->next`,
        // owl/daemon/core.c:42) and retries it when the AW/guard allows.
        let mut pending_tx: Option<PendingFrame> = None;
        // Multicast/broadcast frames (mDNS announce, NDP, broadcast) are
        // spread across every distinct channel of the master hop cycle so a
        // peer listening on any of those channels hears them. owl emits these
        // once on the current channel (`awdl_send_multicast`, core.c:246-277);
        // filin extends that to the full cycle because it channel-hops.
        let mut mcast_spread = crate::schedule::MulticastSpread::new(
            crate::schedule::MULTICAST_SPREAD_CAP,
            crate::schedule::HOP_CYCLE_US,
        );
        // Part A: cache of the latest multicast mDNS announce from the TAP,
        // re-broadcast on every social-channel visit so discovery does not
        // depend on luftlift's ~10s cadence aligning with a 149 hop.
        let mut announce_cache = crate::schedule::AnnounceCache::new(
            crate::schedule::ANNOUNCE_REBROADCAST_MIN_GAP_US,
            crate::schedule::ANNOUNCE_CACHE_TTL_US,
        );

        // FILIN_NEIGHBOR_TABLE.md: tracks which peer MAC→IPv6 mappings filin
        // has installed in the system neighbor table. AWDL does NOT use NDP;
        // filin derives each peer's link-local IPv6 from its source MAC and
        // installs a static neighbor entry so unicast awdl0 connections work
        // (without this, AirDrop's /Discover fails with "No route to host").
        let mut neighbor_table = crate::state::NeighborTable::new();
        let host_iface = links.host_iface.clone();

        // --- localhost introspection HTTP server (FILIN_HTTP_INTROSPECT.md).
        //     Runs on its own OS thread so it can never block the AF_PACKET
        //     hot loop below; the hot loop only bumps atomics and (throttled)
        //     refreshes an RwLock snapshot. ---
        if let Some(addr) = http_addr {
            match std::net::TcpListener::bind(addr) {
                Ok(listener) => {
                    tracing::info!(%addr, "introspection HTTP server listening");
                    let _http_handle = crate::introspect::spawn(introspect.clone(), listener);
                    // _http_handle dropped here intentionally: the thread runs
                    // for the lifetime of the daemon (detached).
                }
                Err(err) => tracing::warn!(
                    ?err,
                    %addr,
                    "introspection HTTP bind failed; continuing without the introspection server"
                ),
            }
        }

        if park {
            tracing::info!(
                "park mode enabled — filin will stay fixed on the top master's anchor channel (no hopping)"
            );
        }
        // Tracks the last park channel we committed (for change detection).
        let mut current_park: Option<u8> = None;

        // Hot-loop introspection state. last_injected_seq tracks the most
        // recently injected AWDL sequence control for /status "master_seq".
        let mut last_injected_seq: u16 = 0;
        // Throttle status/peer snapshot refreshes to ~10 Hz so the RwLock
        // write happens at most every 100 ms — cheap, never blocks the loop.
        let mut last_refresh_us: u64 = 0;
        const REFRESH_PERIOD_US: u64 = 100_000;

        // Consecutive iterations the monitor socket has reported POLLERR/POLLHUP
        // without POLLIN. Reset to 0 on any successful POLLIN read; used to
        // escalate a stuck socket to a clean exit instead of a silent spin.
        let mut consecutive_packet_errors: u32 = 0;

        // Liveness check for the monitor interface. Two failure shapes that the
        // AF_PACKET socket does NOT surface as POLLERR:
        //   * card FLAP — iface left administratively DOWN (no carrier), and
        //   * card UNPLUG/REPLUG — USB re-enumerates, the netdev is recreated
        //     with the SAME name but a NEW ifindex, so our socket (bound to the
        //     old index) is dead even though the iface looks "up" (often
        //     DORMANT). IFF_UP can't see this; the ifindex change can.
        // Poll ~once/second; if the monitor is down, gone, or re-enumerated,
        // return so main()'s retry loop re-runs open_links (reconfigure mode +
        // up + channel, rebind the socket on the new index). Same MAC → awdl0
        // keeps its address, so luftlift isn't disturbed.
        let monitor_ifindex = os::netdev::ifindex(&links.monitor_iface);
        let mut last_iface_check_us: u64 = 0;
        const IFACE_CHECK_PERIOD_US: u64 = 1_000_000;

        loop {
            let now_us = host_time_us();

            if now_us.wrapping_sub(last_iface_check_us) >= IFACE_CHECK_PERIOD_US {
                last_iface_check_us = now_us;
                let cur_ifindex = os::netdev::ifindex(&links.monitor_iface);
                if cur_ifindex == 0
                    || cur_ifindex != monitor_ifindex
                    || !os::netdev::is_up(&links.monitor_iface)
                {
                    tracing::warn!(
                        iface = %links.monitor_iface,
                        was_ifindex = monitor_ifindex,
                        now_ifindex = cur_ifindex,
                        "monitor interface down/gone/re-enumerated (flap or replug); reopening links"
                    );
                    return Err(Error::Netdev);
                }
            }

            // --- FILIN_SYNC_QUALITY.md pivot: single-channel park mode.
            //     Re-evaluate the park channel (the stable top master's
            //     anchor) on the throttled cadence; update_channel below then
            //     only fires when it actually changes. AW timing is kept so
            //     announces still transmit during availability windows. ---
            if park && now_us.wrapping_sub(last_refresh_us) >= REFRESH_PERIOD_US {
                let desired_park = awdl_state.park_channel();
                if current_park != Some(desired_park) {
                    tracing::info!(park_channel = desired_park, "park channel (re)selected");
                    awdl_state.enable_park(desired_park);
                    current_park = Some(desired_park);
                }
            }

            // --- throttled introspection snapshot refresh (~10 Hz) ---
            if now_us.wrapping_sub(last_refresh_us) >= REFRESH_PERIOD_US {
                let tsf_offset_us = bridge.offset_us().unwrap_or(0);
                introspect
                    .set_status(build_status_snapshot(&awdl_state, last_injected_seq, tsf_offset_us, now_us));
                introspect.set_peers(build_peer_views(&awdl_state, now_us));
                last_refresh_us = now_us;
            }

            // --- channel switching (owl awdl_switch_channel, core.c:279) ---
            if announce.switch_due(&awdl_state.sync, now_us) {
                if let Some(channel) = awdl_state.update_channel(now_us) {
                    if let Some(freq) = channel::channel_to_frequency_mhz(channel) {
                        match os::nl80211::set_channel(&links.monitor_iface, u32::from(freq)) {
                            Ok(()) => tracing::debug!(
                                channel,
                                slot = awdl_state.sync.current_eaw(now_us) % 16,
                                "switched monitor channel"
                            ),
                            Err(err) => tracing::warn!(
                                ?err,
                                channel,
                                "failed to switch monitor channel"
                            ),
                        }
                    }
                }
            }

            // --- peer table cleanup (owl awdl_clean_peers, core.c:320) ---
            if announce.clean_due(now_us) {
                let removed = awdl_state.clean_peers(now_us);
                if !removed.is_empty() {
                    tracing::info!(
                        removed = removed.len(),
                        peers = awdl_state.peers.len(),
                        "peer table cleaned"
                    );
                }
                // FILIN_NEIGHBOR_TABLE.md: evict neighbor entries for peers
                // no longer in the table. `removed` is the MACs dropped this
                // cycle; live_macs is everyone still present.
                let live_macs: Vec<[u8; 6]> =
                    awdl_state.peers.iter().map(|p| p.addr).collect();
                for change in neighbor_table.evict_stale(&live_macs) {
                    if let crate::state::NeighborChange::Remove(mac, ipv6) = change {
                        if let Err(err) = os::rtnl::remove_neighbor(&host_iface, ipv6) {
                            tracing::warn!(
                                ?err,
                                mac = %crate::introspect::mac_string(mac),
                                "neighbor remove failed"
                            );
                        } else {
                            tracing::debug!(
                                mac = %crate::introspect::mac_string(mac),
                                "removed neighbor entry for evicted peer"
                            );
                        }
                    }
                }
            }

            // --- multicast spread TX: drain pending multicast/broadcast
            //     frames whose remaining channels include the current hop
            //     channel. Runs every iteration so a freshly-enqueued mDNS
            //     announce is injected on the current channel now AND on each
            //     distinct channel as filin hops through the master cycle
            //     (owl awdl_send_multicast, core.c:246-277, extended). ---
            if !mcast_spread.is_empty() {
                drain_mcast_spread(
                    &links,
                    &awdl_state,
                    &mut mcast_spread,
                    now_us,
                    &introspect,
                );
            }

            // --- Part A: re-broadcast the cached mDNS announce on every
            //     social-channel visit (rate-limited ~1s/channel). Decouples
            //     discovery from luftlift's 10s cadence. Suppressed while a
            //     unicast transfer is pinned (Bug C). ---
            if announce_cache.is_live(now_us)
                && crate::schedule::transfer_active(
                    awdl_state.transfer.as_ref(),
                    now_us,
                    crate::schedule::TRANSFER_IDLE_TIMEOUT_US,
                )
                .is_none()
            {
                rebroadcast_announce(&links, &awdl_state, &mut announce_cache, now_us, &introspect);
            }

            // --- action frame TX: PSF/MIF (owl awdl_send_psf/mif) ---
            for subtype in announce.due(now_us, &awdl_state) {
                let seq = awdl_state.next_data_seq();
                let frame = build_announce_frame(subtype, &awdl_state, seq << 4, now_us);
                match links.packet.send(&frame) {
                    Ok(sent) => {
                        last_injected_seq = seq;
                        tracing::trace!(sent, ?subtype, "injected awdl announce frame");
                    }
                    Err(err) => tracing::trace!(?err, ?subtype, "packet announce send failed"),
                }
            }

            // --- data TX: retry pending frame if AW allows ---
            if let Some(frame) = pending_tx.take() {
                match try_send_data(&links, &awdl_state, now_us, &frame, tx_retransmits) {
                    TxOutcome::Sent => last_injected_seq = frame.seq,
                    // still can't send — keep it pending
                    TxOutcome::Hold => pending_tx = Some(frame),
                    // non-peer unicast — discard so the TAP isn't blocked
                    TxOutcome::Drop => {}
                }
            }

            let mut fds = [
                libc::pollfd {
                    fd: links.packet.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: links.tap.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // Only poll the tap if we don't already have a pending frame.
            if pending_tx.is_some() {
                fds[1].events = 0;
            }
            let poll_timeout_ms = announce.poll_timeout_ms(host_time_us());
            // SAFETY: fds points to two valid pollfd entries for the duration of the call.
            let rc =
                unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, poll_timeout_ms) };
            if rc < 0 {
                return Err(Error::Poll);
            }

            if fds[0].revents & libc::POLLIN != 0 {
                consecutive_packet_errors = 0;
                match links.packet.recv(&mut wlan_buf) {
                    Ok(len) => {
                        let now_us = host_time_us();
                        match rx::parse_action_frame(&wlan_buf[..len], now_us, &mut bridge) {
                            Ok(frame) => {
                                let src = frame.source;
                                let subtype = frame.action.subtype;
                                let rx_time_us = frame.rx_time_us;
                                let rssi_dbm = frame.rssi_dbm;
                                let tlv_count = frame.tlvs.len();
                                // FILIN_AUDIT_FIXES.md #1: RSSI admission
                                // filter (owl rx.c:273-278). Drop weak frames
                                // BEFORE admitting/updating the peer — this is
                                // the upstream cause of master flapping. A
                                // known peer gets grace (-70); an unknown peer
                                // must clear -65 to be discovered.
                                let rssi_ok = disable_rssi_filter || {
                                    let known = awdl_state.peers.get(&src).is_some();
                                    rx::rssi_admits(
                                        rssi_dbm,
                                        known,
                                        rx::RSSI_THRESHOLD_DEFAULT,
                                        rx::RSSI_GRACE_DEFAULT,
                                    )
                                };
                                if rssi_ok && src != links.self_addr {
                                    awdl_state.apply_action(src, subtype, &frame.tlvs, rx_time_us);
                                // FILIN_NEIGHBOR_TABLE.md: install a static
                                // neighbor entry for this peer so unicast
                                // awdl0 connections work. AWDL does not use
                                // NDP — derive IPv6 from MAC (RFC 4291) and
                                // add to the kernel neighbor table. (Self-sourced
                                // monitor reflections are excluded above so we
                                // never install a neighbor for our own MAC.)
                                match neighbor_table.note_peer_seen(&src) {
                                    crate::state::NeighborChange::Add(mac, ipv6) => {
                                        if let Err(err) =
                                            os::rtnl::add_neighbor(&host_iface, ipv6, mac)
                                        {
                                            tracing::warn!(
                                                ?err,
                                                ?mac,
                                                "neighbor add failed; unicast to this peer may fail"
                                            );
                                        } else {
                                            tracing::debug!(
                                                mac = %crate::introspect::mac_string(mac),
                                                "installed neighbor entry for peer"
                                            );
                                        }
                                    }
                                    crate::state::NeighborChange::NoChange => {}
                                    crate::state::NeighborChange::Remove(_, _) => {}
                                }
                                // FILIN_SYNC_QUALITY.md Part A: detect
                                // master_addr vs sync_addr transitions so
                                // /status can prove whether the cluster top
                                // master is stable while we re-parent.
                                let t = awdl_state.note_election_transitions();
                                if t.master_addr_changed {
                                    introspect.counters.inc_master_addr_change();
                                    introspect
                                        .counters
                                        .push_master_mac(crate::introspect::mac_string(
                                            awdl_state.election.master_addr,
                                        ));
                                }
                                if t.sync_addr_changed {
                                    introspect.counters.inc_sync_addr_change();
                                    introspect.counters.inc_master_change();
                                }
                                // AW alignment sample: aligned iff the last
                                // sync error was within ~1 EAW guard band.
                                if let Some(err_tu) = awdl_state.last_sync_error_tu {
                                    introspect
                                        .counters
                                        .record_aw_check(err_tu.abs() <= AW_ALIGN_TOLERANCE_TU);
                                }
                                tracing::trace!(
                                    rx_time_us,
                                    rssi_dbm,
                                    tlv_count,
                                    master = ?awdl_state.election.master_addr,
                                    peers = awdl_state.peers.len(),
                                    channel = awdl_state.current_channel,
                                    "rx awdl action frame"
                                );
                                } // end rssi_ok
                            }
                            Err(err) => match decapsulate_rx_data(&wlan_buf[..len]) {
                                Ok((source, frames)) => {
                                    if awdl_state.peers.get(&source).is_none() {
                                        tracing::trace!(
                                            ?source,
                                            "drop data frame from non-peer"
                                        );
                                    } else {
                                        for frame in &frames {
                                            // Count mDNS (UDP/5353) frames
                                            // delivered to awdl0 by source —
                                            // mdns_rx_other is the direct
                                            // "are we hearing the iPhone?"
                                            // signal.
                                            if let Some(is_self) =
                                                classify_mdns(&frame.bytes, links.self_addr)
                                            {
                                                if is_self {
                                                    introspect.counters.inc_mdns_rx_self();
                                                } else {
                                                    introspect.counters.inc_mdns_rx_other();
                                                }
                                            }
                                            links
                                                .tap
                                                .write_all(&frame.bytes)
                                                .map_err(|_| Error::Io)?;
                                        }
                                        // Bug C: arm the sticky transfer pin so
                                        // filin holds this peer's channel and
                                        // suppresses hopping/re-adoption for
                                        // the duration of the bulk upload.
                                        awdl_state.note_unicast_rx(
                                            source,
                                            awdl_state.current_channel,
                                            now_us,
                                        );
                                        tracing::trace!(
                                            count = frames.len(),
                                            channel = awdl_state.current_channel,
                                            "wrote ethernet frame(s) to tap"
                                        );
                                    }
                                }
                                Err(data_err) => {
                                    if let Some(pcap) = links.pcap.as_mut() {
                                        if let Err(err) =
                                            pcap.write_packet(now_us, &wlan_buf[..len])
                                        {
                                            tracing::trace!(
                                                ?err,
                                                "pcap ignored-frame write failed"
                                            );
                                        }
                                    }
                                    let ieee80211_preview =
                                        ieee80211_payload_preview(&wlan_buf[..len]);
                                    tracing::trace!(
                                        ?err,
                                        ?data_err,
                                        %ieee80211_preview,
                                        "ignored wlan frame"
                                    );
                                }
                            },
                        }
                    }
                    Err(err) => tracing::trace!(?err, "packet recv failed"),
                }
            } else if poll_revents_is_error(fds[0].revents) {
                // Monitor socket in POLLERR/POLLHUP/POLLNVAL (e.g. carl9170 USB
                // reset / firmware -110). poll() returns this immediately every
                // iteration; the old loop ignored it and spun the CPU at 100%
                // with no RX. Drain the pending socket error so a transient
                // condition can clear, back off so we never spin, and exit if
                // it persists past the fatal threshold.
                consecutive_packet_errors = consecutive_packet_errors.saturating_add(1);
                let drained = links.packet.recv(&mut wlan_buf);
                tracing::warn!(
                    revents = fds[0].revents,
                    consecutive = consecutive_packet_errors,
                    drained = ?drained,
                    "monitor packet socket error (POLLERR/POLLHUP); draining and backing off"
                );
                std::thread::sleep(POLL_ERROR_BACKOFF);
                if poll_error_action(consecutive_packet_errors, POLL_ERROR_FATAL_THRESHOLD)
                    == PollErrorAction::Fatal
                {
                    tracing::error!(
                        consecutive = consecutive_packet_errors,
                        "monitor packet socket stuck in error state; exiting so the radio can be recovered and filin restarted"
                    );
                    return Err(Error::Packet);
                }
            }

            if fds[1].revents & libc::POLLIN != 0 {
                let len = links.tap.read(&mut tap_buf).map_err(|_| Error::Io)?;
                if len >= 14 {
                    let dst = mac_from_slice(&tap_buf[0..6]);
                    let src = mac_from_slice(&tap_buf[6..12]);
                    // Count outgoing mDNS announces from luftlift (self) so
                    // /status can show our own announce cadence. Also used
                    // below to gate the re-broadcast cache to mDNS only.
                    let is_self_mdns =
                        matches!(classify_mdns(&tap_buf[..len], links.self_addr), Some(true));
                    if is_self_mdns {
                        introspect.counters.inc_mdns_rx_self();
                    }
                    match awdl::encapsulate_ethernet_payload(&tap_buf[..len], awdl_state.tx_seq) {
                        Ok(body) => {
                            let seq = awdl_state.next_data_seq();
                            // Unicast data frames (TCP/ACK traffic) request
                            // 802.11 ACK + HW retries; multicast/broadcast
                            // (mDNS announce, NDP) stay NOACK.
                            let unicast = dst[0] & 0x01 == 0;
                            let frame = PendingFrame {
                                radiotap_frame: radiotap_prefixed(
                                    ieee80211::build_data_frame(
                                        dst,
                                        src,
                                        AWDL_BSSID,
                                        seq << 4,
                                        &body,
                                    ),
                                    unicast,
                                ),
                                dst,
                                seq,
                            };
                            // Multicast/broadcast frames (mDNS announce,
                            // NDP, broadcast) are spread across every
                            // distinct channel of the master hop cycle so a
                            // peer listening on any channel hears them.
                            // Unicast keeps owl's AW-gated single-send path.
                            if frame.dst[0] & 0x01 == 1 {
                                // Part A: cache the latest mDNS announce for
                                // periodic re-broadcast on social channels,
                                // decoupling discovery from luftlift's 10s
                                // cadence. Cache ONLY mDNS — caching every
                                // multicast frame let lldpd's ~1 Hz LLDP on
                                // awdl0 overwrite the 0.1 Hz mDNS in this
                                // single-slot cache, so the re-broadcast (and
                                // thus discovery) was almost always LLDP, not
                                // the AirDrop announce (live: Mac saw our LLDP
                                // every 1s but mDNS only occasionally).
                                if is_self_mdns {
                                    announce_cache.refresh(
                                        frame.radiotap_frame.clone(),
                                        host_time_us(),
                                    );
                                }
                                if mcast_spread.enqueue(
                                    frame.radiotap_frame,
                                    &awdl_state.channel_sequence,
                                    awdl_state.anchor_channel as u8,
                                    host_time_us(),
                                ) {
                                    tracing::trace!(
                                        pending = mcast_spread.len(),
                                        "enqueued multicast frame for hop-cycle spread"
                                    );
                                    // Inject on the current channel now too
                                    // (it is one of the target channels).
                                    drain_mcast_spread(
                                        &links,
                                        &awdl_state,
                                        &mut mcast_spread,
                                        host_time_us(),
                                        &introspect,
                                    );
                                }
                            } else {
                                match try_send_data(
                                    &links,
                                    &awdl_state,
                                    host_time_us(),
                                    &frame,
                                    tx_retransmits,
                                ) {
                                    TxOutcome::Sent => last_injected_seq = seq,
                                    TxOutcome::Hold => {
                                        pending_tx = Some(frame);
                                        tracing::trace!(
                                            pending = true,
                                            "buffered data frame for later AW-gated TX"
                                        );
                                    }
                                    // non-peer unicast — discard, don't block the TAP
                                    TxOutcome::Drop => {}
                                }
                            }
                        }
                        Err(err) => tracing::trace!(?err, len, "ignored host tap frame"),
                    }
                }
            } else if poll_revents_is_error(fds[1].revents) {
                // awdl0 TAP reported error/hangup (the interface was removed or
                // the fd is invalid). Unlike the monitor socket this cannot
                // recover in place — the TAP is gone — so surface it loudly and
                // exit rather than ignore it and spin (same silent-spin class).
                tracing::error!(
                    revents = fds[1].revents,
                    "awdl0 TAP error/hangup; exiting so filin can be restarted"
                );
                return Err(Error::Tap);
            }
        }
    }

    /// A frame waiting to be injected when the availability window allows.
    /// owl keeps one pending unicast frame in `state->next` (owl/daemon/core.c:42).
    struct PendingFrame {
        radiotap_frame: Vec<u8>,
        dst: [u8; 6],
        /// AWDL sequence control used for this frame, tracked so the
        /// introspection `/status master_seq` reflects the most recent
        /// injection even after a buffered retry.
        seq: u16,
    }

    /// Outcome of an attempt to inject a pending data frame.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TxOutcome {
        /// Injected (or the send failed transiently) — clear the pending slot.
        Sent,
        /// Not sendable yet — keep the frame pending and retry next AW.
        Hold,
        /// Never sendable (non-peer unicast) — discard the frame.
        Drop,
    }

    /// owl `awdl_can_send_unicast_in` decision (owl/src/schedule.c:57-79):
    /// sends if we are inside the guard-free core of an AW AND on the same
    /// channel as the destination peer (owl awdl_same_channel_as_peer). A
    /// non-peer unicast destination is dropped (owl frees the buffer), never
    /// held — see [`TxOutcome::Drop`].
    fn try_send_data(
        links: &Links,
        awdl_state: &crate::state::AwdlState,
        now_us: u64,
        frame: &PendingFrame,
        tx_retransmits: u32,
    ) -> TxOutcome {
        let decision = crate::schedule::data_send_decision(
            &frame.dst,
            awdl_state.current_channel,
            &awdl_state.channel_sequence,
            &awdl_state.peers,
            &awdl_state.sync,
            now_us,
        );
        match decision {
            crate::schedule::DataSendDecision::Drop => {
                tracing::trace!(dst = ?frame.dst, "dropped non-peer unicast data frame");
                return TxOutcome::Drop;
            }
            crate::schedule::DataSendDecision::Hold => return TxOutcome::Hold,
            crate::schedule::DataSendDecision::Send => {}
        }
        match links.packet.send(&frame.radiotap_frame) {
            Ok(sent) => {
                let unicast = frame.dst[0] & 0x01 == 0;
                tracing::trace!(
                    sent,
                    channel = awdl_state.current_channel,
                    multicast = !unicast,
                    "injected awdl data frame"
                );
                // Blind redundancy: the carl9170 does NOT MAC-retransmit
                // injected frames, so re-inject UNICAST frames a few extra
                // times with the 802.11 Retry bit set. The peer's 802.11
                // duplicate-detection (seqno + Retry bit) drops the copies, but
                // if the first is lost on air a copy still arrives — avoiding
                // the slow TCP-timeout recovery that stalled bulk uploads.
                if unicast && tx_retransmits > 0 {
                    let retry = with_retry_bit(&frame.radiotap_frame);
                    for _ in 0..tx_retransmits {
                        let _ = links.packet.send(&retry);
                    }
                }
                TxOutcome::Sent
            }
            Err(err) => {
                // Transient send error — retry on the next AW rather than
                // dropping (the frame is still valid for a known peer).
                tracing::trace!(?err, "data send failed");
                TxOutcome::Hold
            }
        }
    }

    /// Return a copy of a radiotap-prefixed 802.11 frame with the **Retry**
    /// bit (frame-control 0x0800) set, so a re-injected copy looks like a
    /// MAC-layer retransmit and the peer de-dups it. The 802.11 frame-control
    /// is the first 2 bytes after the radiotap header (whose length is the
    /// little-endian u16 at offset 2-3); the Retry bit lives in the high byte.
    fn with_retry_bit(radiotap_frame: &[u8]) -> Vec<u8> {
        let mut copy = radiotap_frame.to_vec();
        if copy.len() >= 4 {
            let rt_len = u16::from_le_bytes([copy[2], copy[3]]) as usize;
            if copy.len() > rt_len + 1 {
                copy[rt_len + 1] |= 0x08; // frame-control bit 11 (Retry)
            }
        }
        copy
    }

    /// Inject any pending multicast-spread frames whose remaining channels
    /// include the current hop channel. Gated by the multicast guard interval
    /// (owl `awdl_can_send_in` with `AWDL_MULTICAST_GUARD_TU`) so we only
    /// inject in the sendable core of the current availability window; if we
    /// are inside the guard, the frames stay queued and are retried on the
    /// next iteration (or the next hop onto that channel). The pure
    /// scheduling decision lives in [`crate::schedule::MulticastSpread`].
    /// Each successful send bumps the per-channel `rebroadcast_counts` counter
    /// so `/status` can show how often filin re-injects on each social channel.
    fn drain_mcast_spread(
        links: &Links,
        awdl_state: &crate::state::AwdlState,
        spread: &mut crate::schedule::MulticastSpread,
        now_us: u64,
        introspect: &crate::introspect::Introspection,
    ) {
        let channel = awdl_state.current_channel;
        if channel == 0 {
            return; // null slot: nothing to transmit
        }
        if crate::schedule::can_send_in(
            &awdl_state.sync,
            now_us,
            crate::schedule::MULTICAST_GUARD_TU,
        ) != 0 {
            return; // inside multicast guard; retry next iteration
        }
        for frame in spread.drain_for_channel(channel, now_us) {
            match links.packet.send(&frame) {
                Ok(sent) => {
                    introspect.counters.inc_rebroadcast(channel);
                    tracing::trace!(
                        sent,
                        channel,
                        pending = spread.len(),
                        "injected spread multicast frame on hop channel"
                    );
                }
                Err(err) => tracing::trace!(?err, channel, "spread multicast send failed"),
            }
        }
    }

    /// Part A: re-inject the cached mDNS announce on the current channel if
    /// the [`AnnounceCache`] permits it (social channel, rate-limit ok, not
    /// expired). Gated by the multicast guard interval like the spread path.
    /// Each successful send bumps the per-channel `rebroadcast_counts` counter.
    fn rebroadcast_announce(
        links: &Links,
        awdl_state: &crate::state::AwdlState,
        cache: &mut crate::schedule::AnnounceCache,
        now_us: u64,
        introspect: &crate::introspect::Introspection,
    ) {
        let channel = awdl_state.current_channel;
        if channel == 0 {
            return;
        }
        if crate::schedule::can_send_in(
            &awdl_state.sync,
            now_us,
            crate::schedule::MULTICAST_GUARD_TU,
        ) != 0 {
            return; // inside multicast guard; retry next iteration
        }
        // Adaptive allowed set: social channels PLUS the adopted master's
        // anchor channel (e.g. 52 on a DFS-anchored cluster), so the cached
        // announce is re-broadcast on the anchor too.
        let allowed = crate::schedule::rebroadcast_channels(awdl_state.master_anchor);
        if let Some(frame) = cache.rebroadcast(channel, now_us, &allowed) {
            match links.packet.send(frame) {
                Ok(sent) => {
                    introspect.counters.inc_rebroadcast(channel);
                    tracing::debug!(
                        sent,
                        channel,
                        "re-broadcast cached mDNS announce on social channel"
                    );
                }
                Err(err) => {
                    tracing::trace!(?err, channel, "announce re-broadcast send failed")
                }
            }
        }
    }

    fn mac_from_slice(bytes: &[u8]) -> [u8; 6] {
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&bytes[..6]);
        mac
    }

    const TU_US: u64 = 1024;
    const PSF_INTERVAL_TU: u64 = 110;
    /// owl `PSF_INTERVAL_MASTER_TU` is 110; slave is 440. owl clean interval
    /// (`PEERS_DEFAULT_CLEAN_INTERVAL`) = 1 000 000 us (owl/src/peers.c:29).
    const CLEAN_INTERVAL_US: u64 = 1_000_000;
    /// Sync-error tolerance (TU) below which an availability window counts as
    /// "aligned" for `/status aw_alignment_pct` (FILIN_SYNC_QUALITY.md). Half
    /// an AW guard (8 TU) — loose enough to ride EMA jitter, tight enough that
    /// a runaway bridge shows up as 0%.
    const AW_ALIGN_TOLERANCE_TU: i64 = 8;
    const AWDL_BROADCAST: [u8; 6] = [0xff; 6];
    const AWDL_BSSID: [u8; 6] = [0x00, 0x25, 0x00, 0xff, 0x94, 0x73];

    #[derive(Debug, PartialEq, Eq)]
    struct AnnounceScheduler {
        next_psf_us: u64,
        next_mif_us: u64,
        next_clean_us: u64,
        next_switch_us: u64,
    }

    impl AnnounceScheduler {
        fn new(now_us: u64) -> Self {
            Self {
                next_psf_us: now_us + PSF_INTERVAL_TU * TU_US,
                // owl arms the MIF timer to fire immediately the first time
                // (core.c:419), then awdl_send_mif reschedules to mid-EAW.
                next_mif_us: now_us,
                next_clean_us: now_us + CLEAN_INTERVAL_US,
                // owl fires awdl_switch_channel immediately on start
                // (core.c:392), then rearms to next AW boundary.
                next_switch_us: now_us,
            }
        }

        fn poll_timeout_ms(&self, now_us: u64) -> i32 {
            let next = self
                .next_psf_us
                .min(self.next_mif_us)
                .min(self.next_clean_us)
                .min(self.next_switch_us);
            let delta_us = next.saturating_sub(now_us);
            delta_us.div_ceil(1_000).min(i32::MAX as u64) as i32
        }

        /// owl `awdl_clean_peers` timer (owl/daemon/core.c:320-336) fires every
        /// `CLEAN_INTERVAL_US`.
        fn clean_due(&mut self, now_us: u64) -> bool {
            if now_us >= self.next_clean_us {
                while self.next_clean_us <= now_us {
                    self.next_clean_us += CLEAN_INTERVAL_US;
                }
                true
            } else {
                false
            }
        }

        /// owl `awdl_switch_channel` timer (owl/daemon/core.c:279-308): fires at
        /// every AW boundary and rearms to `next_aw_us`.
        fn switch_due(&mut self, sync: &crate::sync::SyncState, now_us: u64) -> bool {
            if now_us >= self.next_switch_us {
                self.next_switch_us = now_us + sync.next_aw_us(now_us);
                true
            } else {
                false
            }
        }

        /// Returns the action subtypes whose timers have fired, rescheduling
        /// each. MIF follows owl `awdl_send_mif` (owl/daemon/core.c:184-200): it
        /// only fires when our current channel slot is non-null and is rearmed
        /// to the middle of the next EAW. PSF fires every `PSF_INTERVAL_TU`
        /// (owl `awdl_send_psf`, core.c:178-182).
        fn due(
            &mut self,
            now_us: u64,
            awdl_state: &crate::state::AwdlState,
        ) -> Vec<awdl::ActionSubtype> {
            let mut due = Vec::new();
            if now_us >= self.next_mif_us {
                let slot =
                    usize::from(awdl_state.sync.current_eaw(now_us) % 16);
                let channel = crate::schedule::slot_channel(&awdl_state.channel_sequence, slot);
                if channel > 0 {
                    due.push(awdl::ActionSubtype::Mif);
                }
                self.next_mif_us = next_mif_us(&awdl_state.sync, now_us);
            }
            if now_us >= self.next_psf_us {
                due.push(awdl::ActionSubtype::Psf);
                while self.next_psf_us <= now_us {
                    self.next_psf_us += PSF_INTERVAL_TU * TU_US;
                }
            }
            due
        }
    }

    /// owl `awdl_send_mif` rearms to the middle of the next EAW:
    /// `next_aw + eaw_len/2` (owl/daemon/core.c:199).
    fn next_mif_us(sync: &crate::sync::SyncState, now_us: u64) -> u64 {
        let next_aw = sync.next_aw_us(now_us);
        let eaw_len = u64::from(sync.presence_mode) * u64::from(sync.aw_period_tu) * TU_US;
        now_us + next_aw + eaw_len / 2
    }

    fn build_announce_frame(
        subtype: awdl::ActionSubtype,
        awdl_state: &crate::state::AwdlState,
        sequence_control: u16,
        now_us: u64,
    ) -> Vec<u8> {
        let tlvs = build_announce_tlvs(subtype, awdl_state, now_us);
        let action = awdl::build_action_body(subtype, now_us as u32, now_us as u32, &tlvs);
        let frame = ieee80211::build_management_action_frame(
            AWDL_BROADCAST,
            awdl_state.self_addr,
            AWDL_BSSID,
            sequence_control,
            &action,
        );
        // AWDL action frames are broadcast → never ACKed.
        radiotap_prefixed(frame, false)
    }

    fn build_announce_tlvs(
        subtype: awdl::ActionSubtype,
        awdl_state: &crate::state::AwdlState,
        now_us: u64,
    ) -> Vec<u8> {
        let mut tlvs = Vec::new();
        push_tlv(
            &mut tlvs,
            4,
            &build_sync_parameters_tlv_value(awdl_state, now_us),
        );
        push_tlv(
            &mut tlvs,
            5,
            &build_election_parameters_tlv_value(&awdl_state.election),
        );
        push_tlv(
            &mut tlvs,
            18,
            &build_channel_sequence_tlv_value(&awdl_state.channel_sequence),
        );
        push_tlv(
            &mut tlvs,
            24,
            &build_election_parameters_v2_tlv_value(&awdl_state.election),
        );
        push_tlv(&mut tlvs, 6, &build_service_parameters_tlv_value());
        if subtype == awdl::ActionSubtype::Mif {
            push_tlv(&mut tlvs, 7, &build_ht_capabilities_tlv_value());
            push_tlv(&mut tlvs, 16, &build_arpa_tlv_value(&awdl_state.name));
        }
        push_tlv(
            &mut tlvs,
            12,
            &build_data_path_state_tlv_value(awdl_state.self_addr, awdl_state.anchor_channel),
        );
        push_tlv(&mut tlvs, 21, &build_version_tlv_value(awdl_state.version, awdl_state.dev_class));
        tlvs
    }

    fn radiotap_prefixed(mut frame: Vec<u8>, want_ack: bool) -> Vec<u8> {
        let mut prefixed = radiotap::build_tx_header(want_ack);
        prefixed.append(&mut frame);
        prefixed
    }

    fn push_tlv(out: &mut Vec<u8>, kind: u8, value: &[u8]) {
        out.push(kind);
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(value);
    }

    fn build_sync_parameters_tlv_value(
        awdl_state: &crate::state::AwdlState,
        now_us: u64,
    ) -> Vec<u8> {
        let sync = &awdl_state.sync;
        let next_aw_tu = sync.next_aw_tu(now_us);
        let aw_period = sync.aw_period_tu;
        let presence_mode = sync.presence_mode as u8;
        let eaw_len_tu = u16::from(presence_mode) * aw_period;
        let elapsed_in_eaw_tu = eaw_len_tu.saturating_sub(next_aw_tu);
        let remaining_aw_length = aw_period.saturating_sub(elapsed_in_eaw_tu);
        let channel = awdl_state.anchor_channel as u8;
        let mut value = Vec::with_capacity(73);
        value.push(channel); // next_aw_channel
        value.extend_from_slice(&next_aw_tu.to_le_bytes()); // tx_down_counter
        value.push(channel); // master_channel
        value.push(0); // guard_time
        value.extend_from_slice(&aw_period.to_le_bytes()); // aw_period
        value.extend_from_slice(&(PSF_INTERVAL_TU as u16).to_le_bytes()); // af_period
        value.extend_from_slice(&0x1800u16.to_le_bytes()); // flags
        value.extend_from_slice(&aw_period.to_le_bytes()); // aw_ext_length
        value.extend_from_slice(&aw_period.to_le_bytes()); // aw_com_length
        value.extend_from_slice(&remaining_aw_length.to_le_bytes());
        value.push(presence_mode - 1); // min_ext
        value.push(presence_mode - 1); // max_ext_multicast
        value.push(presence_mode - 1); // max_ext_unicast
        value.push(presence_mode - 1); // max_ext_af
        value.extend_from_slice(&awdl_state.election.master_addr); // master_addr
        value.push(presence_mode); // presence_mode
        value.push(0); // reserved
        value.extend_from_slice(&sync.current_aw(now_us).to_le_bytes()); // next_aw_seq
        value.extend_from_slice(&sync.current_aw(now_us).to_le_bytes()); // ap_alignment
        value.extend_from_slice(&build_channel_sequence_value(&awdl_state.channel_sequence));
        value.extend_from_slice(&[0, 0]); // padding
        value
    }

    fn build_channel_sequence_tlv_value(channel_sequence: &[[u8; 2]; 16]) -> Vec<u8> {
        let mut value = build_channel_sequence_value(channel_sequence);
        value.extend_from_slice(&[0, 0, 0]); // padding (owl awdl_init_chanseq_tlv)
        value
    }

    /// owl `awdl_init_chanseq` (owl/src/tx.c:80-96): count=15, encoding=3
    /// (OPCLASS), duplicate_count=0, step_count=3, fill=0xffff, then 16
    /// `(chan_num, opclass)` pairs.
    fn build_channel_sequence_value(channel_sequence: &[[u8; 2]; 16]) -> Vec<u8> {
        let mut value = Vec::with_capacity(38);
        value.extend_from_slice(&[15, 3, 0, 3]);
        value.extend_from_slice(&0xffffu16.to_le_bytes());
        for slot in channel_sequence.iter() {
            value.extend_from_slice(slot);
        }
        value
    }

    /// owl `awdl_init_election_params_tlv` (owl/src/tx.c:166-186): flags, id,
    /// distancetop(=height), unknown, master_addr, master_metric, self_metric.
    fn build_election_parameters_tlv_value(
        election: &crate::election::ElectionState,
    ) -> Vec<u8> {
        let mut value = Vec::with_capacity(21);
        value.push(0); // flags
        value.extend_from_slice(&0u16.to_le_bytes()); // id
        value.push(election.height as u8); // distancetop
        value.push(0); // unknown
        value.extend_from_slice(&election.master_addr);
        value.extend_from_slice(&election.master_metric.to_le_bytes());
        value.extend_from_slice(&election.self_metric.to_le_bytes());
        value.extend_from_slice(&[0, 0]); // pad
        value
    }

    /// owl `awdl_init_election_params_v2_tlv` (owl/src/tx.c:188-206).
    fn build_election_parameters_v2_tlv_value(
        election: &crate::election::ElectionState,
    ) -> Vec<u8> {
        let mut value = Vec::with_capacity(40);
        value.extend_from_slice(&election.master_addr);
        value.extend_from_slice(&election.sync_addr);
        value.extend_from_slice(&election.master_counter.to_le_bytes());
        value.extend_from_slice(&election.height.to_le_bytes()); // distance_to_master
        value.extend_from_slice(&election.master_metric.to_le_bytes());
        value.extend_from_slice(&election.self_metric.to_le_bytes());
        value.extend_from_slice(&0u32.to_le_bytes()); // unknown
        value.extend_from_slice(&0u32.to_le_bytes()); // reserved
        value.extend_from_slice(&election.self_counter.to_le_bytes());
        value
    }

    fn build_service_parameters_tlv_value() -> [u8; 9] {
        [0; 9]
    }

    fn build_ht_capabilities_tlv_value() -> [u8; 8] {
        [0x00, 0x00, 0xce, 0x11, 0x1b, 0xff, 0x00, 0x00]
    }

    fn build_arpa_tlv_value(name: &str) -> Vec<u8> {
        let mut value = Vec::with_capacity(2 + name.len() + 2);
        value.push(3); // flags
        value.push(name.len() as u8);
        value.extend_from_slice(name.as_bytes());
        value.extend_from_slice(&0xc00cu16.to_be_bytes()); // .local
        value
    }

    fn build_data_path_state_tlv_value(self_addr: [u8; 6], anchor_channel: u16) -> Vec<u8> {
        let mut value = Vec::with_capacity(15);
        value.extend_from_slice(&0x8f24u16.to_le_bytes());
        value.extend_from_slice(b"X0\0");
        value.extend_from_slice(&social_channel_bits(anchor_channel).to_le_bytes());
        value.extend_from_slice(&self_addr);
        value.extend_from_slice(&0u16.to_le_bytes());
        value
    }

    fn build_version_tlv_value(version: u8, devclass: u8) -> [u8; 2] {
        [version, devclass]
    }

    fn social_channel_bits(anchor_channel: u16) -> u16 {
        match anchor_channel {
            6 => 0x0001,
            44 => 0x0002,
            149 => 0x0004,
            _ => 0,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum DataParseError {
        Radiotap(radiotap::RadiotapError),
        Fcs(rx::ParseError),
        Ieee80211(ieee80211::ParseError),
        Awdl(awdl::ParseError),
    }

    /// Returns the source MAC and one or more decapsulated Ethernet frames.
    /// A-MSDU frames yield multiple Ethernet frames (owl awdl_rx_data_amsdu).
    fn decapsulate_rx_data(frame: &[u8]) -> Result<([u8; 6], Vec<awdl::EthernetFrame>), DataParseError> {
        let radiotap = radiotap::parse_header(frame).map_err(DataParseError::Radiotap)?;
        let ieee80211_payload =
            rx::ieee80211_payload_without_fcs(&radiotap).map_err(DataParseError::Fcs)?;
        let mac =
            ieee80211::parse_mac_header(ieee80211_payload).map_err(DataParseError::Ieee80211)?;
        // owl checks A-MSDU present bit in the QoS control field (rx.c:540).
        let is_amsdu = mac
            .qos_control
            .map(|q| q & ieee80211::QOS_A_MSDU_PRESENT != 0)
            .unwrap_or(false);
        let frames = if is_amsdu {
            awdl::decapsulate_amsdu_data_frames(mac.body).map_err(DataParseError::Awdl)?
        } else {
            vec![
                awdl::decapsulate_data_frame(mac.body, mac.source, mac.destination)
                    .map_err(DataParseError::Awdl)?,
            ]
        };
        Ok((mac.source, frames))
    }

    fn ieee80211_payload_preview(frame: &[u8]) -> String {
        match radiotap::parse_header(frame) {
            Ok(header) => match rx::ieee80211_payload_without_fcs(&header) {
                Ok(payload) => hex_preview(payload, 64),
                Err(_) => hex_preview(header.payload, 64),
            },
            Err(_) => hex_preview(frame, 64),
        }
    }

    fn hex_preview(bytes: &[u8], limit: usize) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let len = bytes.len().min(limit);
        let mut out = String::with_capacity(len.saturating_mul(3).saturating_sub(1));
        for (index, byte) in bytes.iter().copied().take(limit).enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    fn host_time_us() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is valid writable memory for clock_gettime.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if rc < 0 {
            return 0;
        }
        (ts.tv_sec as u64) * 1_000_000 + (ts.tv_nsec as u64) / 1_000
    }

    /// Build an introspection [`crate::introspect::StatusSnapshot`] from the
    /// live AWDL state + the most-recently-injected frame's sequence control.
    /// The runtime calls this on a throttled cadence and feeds it to
    /// [`crate::introspect::Introspection::set_status`].
    /// Build an introspection [`crate::introspect::StatusSnapshot`] from the
    /// live AWDL state + the most-recently-injected frame's sequence control.
    /// The runtime calls this on a throttled cadence and feeds it to
    /// [`crate::introspect::Introspection::set_status`]. `tsf_offset_us` is
    /// sampled from the [`TsftBridge`] by the caller; `now_us` drives
    /// `master_age_ms` (FILIN_SYNC_QUALITY.md Part A).
    fn build_status_snapshot(
        state: &crate::state::AwdlState,
        last_injected_seq: u16,
        tsf_offset_us: i64,
        now_us: u64,
    ) -> crate::introspect::StatusSnapshot {
        crate::introspect::StatusSnapshot {
            current_channel: state.current_channel,
            master_mac: crate::introspect::mac_string(state.election.master_addr),
            master_seq: last_injected_seq,
            synced: state.election.sync_addr != state.self_addr,
            peer_count: state.peers.len(),
            master_anchor: state.master_anchor,
            tsf_offset_us,
            master_age_ms: now_us.saturating_sub(state.master_adopted_at_us) / 1_000,
            sync_addr_mac: crate::introspect::mac_string(state.election.sync_addr),
        }
    }

    /// Build per-peer introspection views for `GET /peers` from the live
    /// `AwdlState`. `decoded_chanseq` is each slot's channel number; the
    /// per-peer `current_channel` is filin's own (we hop co-channel with the
    /// peers we can hear).
    fn build_peer_views(
        state: &crate::state::AwdlState,
        now_us: u64,
    ) -> Vec<crate::introspect::PeerView> {
        state
            .peers
            .iter()
            .map(|peer| crate::introspect::PeerView {
                mac: crate::introspect::mac_string(peer.addr),
                last_seen_ms_ago: now_us.saturating_sub(peer.last_update_us) / 1_000,
                decoded_chanseq: peer.sequence.iter().map(|slot| slot[0]).collect(),
                current_channel: state.current_channel,
            })
            .collect()
    }

    /// Classify an Ethernet frame as mDNS and report whether the source MAC is
    /// `self_addr`. Returns `Some(true)` for our own mDNS, `Some(false)` for a
    /// peer's mDNS, `None` for non-mDNS (not IP, not UDP, or port != 5353).
    /// Handles IPv4 (with the IHL-driven header length) and IPv6. Used by the
    /// hot loop to bump `mdns_rx_self` / `mdns_rx_other` on the introspection
    /// counters — directly answers "are we hearing the iPhone at all?".
    fn classify_mdns(frame: &[u8], self_addr: [u8; 6]) -> Option<bool> {
        const MDNS_PORT: u16 = 5353;
        const PROTO_UDP: u8 = 17;
        if frame.len() < 14 {
            return None;
        }
        let src_mac: [u8; 6] = frame[6..12].try_into().ok()?;
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        let dst_port = match ethertype {
            0x0800 => {
                // IPv4: protocol at offset 23, IHL-driven header length.
                let ihl = usize::from(frame.get(14)? & 0x0f) * 4;
                if ihl < 20 {
                    return None;
                }
                let proto = *frame.get(23)?;
                if proto != PROTO_UDP {
                    return None;
                }
                let udp_start = 14 + ihl;
                u16::from_be_bytes([
                    *frame.get(udp_start + 2)?,
                    *frame.get(udp_start + 3)?,
                ])
            }
            0x86dd => {
                // IPv6: next header at offset 20, fixed 40-byte header.
                let next_header = *frame.get(20)?;
                if next_header != PROTO_UDP {
                    return None;
                }
                let udp_start = 14 + 40;
                u16::from_be_bytes([
                    *frame.get(udp_start + 2)?,
                    *frame.get(udp_start + 3)?,
                ])
            }
            _ => return None,
        };
        if dst_port != MDNS_PORT {
            return None;
        }
        Some(src_mac == self_addr)
    }

    /// owl `gethostname` for the ARPA TLV peer name (owl/daemon/core.c:362-364).
    fn hostname() -> String {
        let mut buf = [0u8; 256];
        // SAFETY: buf is valid writable memory for gethostname.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc < 0 {
            return String::from("filin");
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn poll_revents_error_bits_detected() {
            // POLLERR/POLLHUP/POLLNVAL each signal an error/hangup, not data.
            assert!(poll_revents_is_error(libc::POLLERR));
            assert!(poll_revents_is_error(libc::POLLHUP));
            assert!(poll_revents_is_error(libc::POLLNVAL));
            // Error bits combined with POLLIN still count as an error.
            assert!(poll_revents_is_error(libc::POLLIN | libc::POLLERR));
            // POLL_ERR_BITS is exactly the union of the three error bits.
            assert_eq!(POLL_ERR_BITS, libc::POLLERR | libc::POLLHUP | libc::POLLNVAL);
        }

        #[test]
        fn poll_revents_plain_pollin_is_not_error() {
            assert!(!poll_revents_is_error(libc::POLLIN));
            assert!(!poll_revents_is_error(0));
        }

        #[test]
        fn poll_error_action_tolerates_transients_then_escalates() {
            // Below threshold: keep running (drain + retry the transient).
            assert_eq!(poll_error_action(0, 200), PollErrorAction::Continue);
            assert_eq!(poll_error_action(199, 200), PollErrorAction::Continue);
            // At/above threshold: fatal — exit so the radio can be recovered.
            assert_eq!(poll_error_action(200, 200), PollErrorAction::Fatal);
            assert_eq!(poll_error_action(5_000, 200), PollErrorAction::Fatal);
            // The shipped default threshold uses the same Continue/Fatal split.
            assert_eq!(
                poll_error_action(POLL_ERROR_FATAL_THRESHOLD, POLL_ERROR_FATAL_THRESHOLD),
                PollErrorAction::Fatal
            );
        }

        #[test]
        fn scheduler_fires_psf_before_mif() {
            // MIF is armed immediately (now) then rescheduled to mid next-EAW.
            // PSF fires one PSF_INTERVAL (110 TU = 112640 us) after start.
            let state =
                crate::state::AwdlState::new([0x02, 1, 2, 3, 4, 5], "filin".into(), 44, 1_000_000);
            let mut scheduler = AnnounceScheduler::new(1_000_000);

            // MIF fires immediately; next MIF lands at mid next-EAW:
            //   next_aw(65536) + eaw_len/2(32768) = 98304 us after start.
            assert_eq!(scheduler.due(1_000_000, &state), vec![awdl::ActionSubtype::Mif]);
            assert!(scheduler.due(1_050_000, &state).is_empty());
            assert_eq!(scheduler.due(1_098_304, &state), vec![awdl::ActionSubtype::Mif]);
            // PSF lands at 1_000_000 + 110*1024 = 1_112_640.
            let due = scheduler.due(1_112_640, &state);
            assert!(due.contains(&awdl::ActionSubtype::Psf));
        }

        fn demo_state() -> crate::state::AwdlState {
            crate::state::AwdlState::new(
                [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                "filin".into(),
                44,
                1_000_000,
            )
        }

        #[test]
        fn builds_broadcast_psf_announce_frame() {
            let state = demo_state();
            let frame = build_announce_frame(awdl::ActionSubtype::Psf, &state, 0x0120, 1_000_000);
            let radiotap = radiotap::parse_header(&frame).expect("valid tx radiotap header");
            // 13-byte TX header now (rate + TX-flags + data-retries fields).
            assert_eq!(radiotap.header_len, 13);
            let mac = ieee80211::parse_mac_header(radiotap.payload).expect("valid action header");
            let action = awdl::parse_action_frame(mac.body).expect("valid awdl action");
            let tlvs = awdl::parse_tlvs(action.tlvs).expect("valid announce TLVs");

            assert_eq!(mac.frame_control, 0x00d0);
            assert_eq!(mac.destination, [0xff; 6]);
            assert_eq!(mac.source, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
            assert_eq!(mac.bssid, AWDL_BSSID);
            assert_eq!(mac.sequence_control, 0x0120);
            assert_eq!(action.subtype, awdl::ActionSubtype::Psf);
            assert!(tlvs.iter().any(|tlv| tlv.kind == 4));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 5));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 18));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 24));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 6));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 12));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 21));
            assert!(!tlvs.iter().any(|tlv| tlv.kind == 7));
            assert!(!tlvs.iter().any(|tlv| tlv.kind == 16));
            // chanseq TLV now uses owl OPCLASS encoding (3) with opclass byte 0x80.
            let chanseq = tlvs.iter().find(|t| t.kind == 18).unwrap().value;
            assert_eq!(chanseq[1], 3); // encoding = OPCLASS
            assert_eq!(&chanseq[6..8], &[44, 0x80]); // first slot (chan 44, opclass 0x80)
        }

        #[test]
        fn announce_election_tlvs_carry_real_state() {
            let mut state = demo_state();
            // adopt a peer as master so the election state is non-default
            let peer_addr = [0x02, 0, 0, 0, 0, 9];
            let mut v2 = vec![0u8; 40];
            v2[0..6].copy_from_slice(&peer_addr);
            v2[6..12].copy_from_slice(&peer_addr);
            v2[12..16].copy_from_slice(&77u32.to_le_bytes());
            v2[16..20].copy_from_slice(&1u32.to_le_bytes()); // distance -> height
            v2[20..24].copy_from_slice(&333u32.to_le_bytes());
            v2[24..28].copy_from_slice(&60u32.to_le_bytes());
            v2[36..40].copy_from_slice(&5u32.to_le_bytes());
            let version = awdl::Tlv { kind: 21, value: &[0x34, 1] };
            let v2_tlv = awdl::Tlv { kind: 24, value: &v2 };
            state.apply_action(peer_addr, awdl::ActionSubtype::Mif, &[version, v2_tlv], 1_100_000);

            let frame = build_announce_frame(awdl::ActionSubtype::Psf, &state, 0, 1_200_000);
            let radiotap = radiotap::parse_header(&frame).expect("valid tx radiotap");
            let mac = ieee80211::parse_mac_header(radiotap.payload).expect("valid action header");
            let action = awdl::parse_action_frame(mac.body).expect("valid awdl action");
            let tlvs = awdl::parse_tlvs(action.tlvs).expect("valid announce TLVs");

            // sync-params TLV master_addr must be the elected master, not self.
            let sync = tlvs.iter().find(|t| t.kind == 4).unwrap().value;
            assert_eq!(&sync[21..27], &peer_addr);
            // election v2 TLV reflects adopted master/counter/height.
            let ev2 = tlvs.iter().find(|t| t.kind == 24).unwrap().value;
            assert_eq!(&ev2[0..6], &peer_addr); // master_addr
            assert_eq!(&ev2[6..12], &peer_addr); // sync_addr
            assert_eq!(u32::from_le_bytes(ev2[12..16].try_into().unwrap()), 77); // master_counter
        }

        #[test]
        fn builds_mif_with_owl_extra_tlvs() {
            let state = demo_state();
            let frame = build_announce_frame(awdl::ActionSubtype::Mif, &state, 0x0120, 1_000_000);
            let radiotap = radiotap::parse_header(&frame).expect("valid tx radiotap header");
            let mac = ieee80211::parse_mac_header(radiotap.payload).expect("valid action header");
            let action = awdl::parse_action_frame(mac.body).expect("valid awdl action");
            let tlvs = awdl::parse_tlvs(action.tlvs).expect("valid announce TLVs");

            assert_eq!(action.subtype, awdl::ActionSubtype::Mif);
            assert!(tlvs.iter().any(|tlv| tlv.kind == 7));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 16));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 12));
            assert!(tlvs.iter().any(|tlv| tlv.kind == 21));
        }

        #[test]
        fn formats_hex_preview_with_limit() {
            assert_eq!(hex_preview(&[0xde, 0xad, 0xbe, 0xef], 3), "de ad be");
        }

        #[test]
        fn previews_payload_after_radiotap_header() {
            let mut frame = vec![
                0x00, 0x00, 0x08, 0x00, // radiotap version, pad, len
                0x00, 0x00, 0x00, 0x00, // no present fields
                0xd0, 0x00,
            ];
            frame.extend(0u8..80);

            assert_eq!(ieee80211_payload_preview(&frame).split(' ').count(), 64);
            assert!(ieee80211_payload_preview(&frame).starts_with("d0 00 00 01"));
        }

        #[test]
        fn status_snapshot_carries_state_and_last_injected_seq() {
            let mut state = crate::state::AwdlState::new(
                [0x02, 0, 0, 0, 0, 1],
                "filin".into(),
                44,
                1_000_000,
            );
            state.current_channel = 44;
            state.peers.touch([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee], 1_000_000);

            let snapshot = build_status_snapshot(&state, 17, 500, 1_500_000);

            assert_eq!(snapshot.current_channel, 44);
            // self-master initially
            assert_eq!(snapshot.master_mac, "02:00:00:00:00:01");
            assert_eq!(snapshot.master_seq, 17);
            assert!(!snapshot.synced);
            assert_eq!(snapshot.peer_count, 1);
            assert_eq!(snapshot.tsf_offset_us, 500);
            assert_eq!(snapshot.master_age_ms, 500); // (1.5M - 1M) / 1000
        }

        #[test]
        fn status_snapshot_marks_synced_when_external_master_adopted() {
            let mut state = crate::state::AwdlState::new(
                [0x02, 0, 0, 0, 0, 1],
                "filin".into(),
                44,
                1_000_000,
            );
            // pretend an external master was elected
            state.election.sync_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.election.master_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            let snapshot = build_status_snapshot(&state, 0, 0, 1_000_000);

            assert_eq!(snapshot.master_mac, "02:aa:bb:cc:dd:ee");
            assert!(snapshot.synced);
        }

        #[test]
        fn peer_views_carry_chanseq_chan_ms_ago() {
            let mut state = crate::state::AwdlState::new(
                [0x02, 0, 0, 0, 0, 1],
                "filin".into(),
                44,
                1_000_000,
            );
            let peer_addr = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
            state.peers.touch(peer_addr, 1_000_000);
            {
                let p = state.peers.get_mut(&peer_addr).unwrap();
                p.sequence[0] = [44, 0x80];
                p.sequence[1] = [149, 0x80];
            }
            state.current_channel = 44;

            let views = build_peer_views(&state, 1_500_000);
            assert_eq!(views.len(), 1);
            let v = &views[0];
            assert_eq!(v.mac, "02:aa:bb:cc:dd:ee");
            assert_eq!(v.last_seen_ms_ago, 500); // 500_000 us = 500 ms
            assert_eq!(v.current_channel, 44);
            assert_eq!(v.decoded_chanseq[0], 44);
            assert_eq!(v.decoded_chanseq[1], 149);
        }

        /// Build a minimal Ethernet/IPv4/UDP frame with the given src MAC and
        /// UDP dst port. IPv4 header is the 20-byte fixed part (IHL=5).
        fn ipv4_udp_frame(src_mac: [u8; 6], dst_port: u16) -> Vec<u8> {
            let mut f = Vec::new();
            f.extend_from_slice(&[0xff; 6]); // dst broadcast
            f.extend_from_slice(&src_mac);
            f.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype IPv4
            // IPv4 header (20 bytes)
            f.push(0x45); // ver=4, IHL=5
            f.push(0x00); // DSCP/ECN
            f.extend_from_slice(&0u16.to_be_bytes()); // total length
            f.extend_from_slice(&0u16.to_be_bytes()); // id
            f.extend_from_slice(&0u16.to_be_bytes()); // flags/frag
            f.push(64); // TTL
            f.push(17); // protocol = UDP (offset 23)
            f.extend_from_slice(&0u16.to_be_bytes()); // checksum
            f.extend_from_slice(&[224, 0, 0, 251]); // src IP
            f.extend_from_slice(&[224, 0, 0, 251]); // dst IP
            // UDP header
            f.extend_from_slice(&5353u16.to_be_bytes()); // src port
            f.extend_from_slice(&dst_port.to_be_bytes()); // dst port
            f.extend_from_slice(&0u16.to_be_bytes()); // length
            f.extend_from_slice(&0u16.to_be_bytes()); // checksum
            f.extend_from_slice(b"payload");
            f
        }

        /// Build a minimal Ethernet/IPv6/UDP frame with the given src MAC and
        /// UDP dst port.
        fn ipv6_udp_frame(src_mac: [u8; 6], dst_port: u16) -> Vec<u8> {
            let mut f = Vec::new();
            f.extend_from_slice(&[0x33, 0x33, 0x00, 0x00, 0x00, 0xfb]); // IPv6 mcast dst
            f.extend_from_slice(&src_mac);
            f.extend_from_slice(&0x86ddu16.to_be_bytes()); // ethertype IPv6
            // IPv6 header (40 bytes)
            f.extend_from_slice(&0x60u32.to_be_bytes()); // version=6, TC=0, FL=0
            f.extend_from_slice(&0u16.to_be_bytes()); // payload length
            f.extend_from_slice(&[0]); // next header placeholder (set below at offset 20)
            f.push(64); // hop limit
            f.extend_from_slice(&[0xfe; 16]); // src IP
            f.extend_from_slice(&[0xff; 16]); // dst IP (ff02::fb)
            // fix next header at offset 20 (14+6) to UDP (17)
            f[20] = 17;
            // UDP header
            f.extend_from_slice(&5353u16.to_be_bytes()); // src port
            f.extend_from_slice(&dst_port.to_be_bytes()); // dst port
            f.extend_from_slice(&0u16.to_be_bytes()); // length
            f.extend_from_slice(&0u16.to_be_bytes()); // checksum
            f.extend_from_slice(b"payload");
            f
        }

        #[test]
        fn classify_mdns_identifies_ipv4_5353_by_source_mac() {
            let self_mac = [0x02, 0, 0, 0, 0, 1];
            let other_mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            assert_eq!(
                classify_mdns(&ipv4_udp_frame(self_mac, 5353), self_mac),
                Some(true)
            );
            assert_eq!(
                classify_mdns(&ipv4_udp_frame(other_mac, 5353), self_mac),
                Some(false)
            );
        }

        #[test]
        fn classify_mdns_handles_ipv6_and_non_mdns_ports() {
            let self_mac = [0x02, 0, 0, 0, 0, 1];
            let other_mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

            assert_eq!(
                classify_mdns(&ipv6_udp_frame(other_mac, 5353), self_mac),
                Some(false)
            );
            // Not mDNS port → None
            assert_eq!(classify_mdns(&ipv4_udp_frame(other_mac, 1234), self_mac), None);
            assert_eq!(classify_mdns(&ipv6_udp_frame(other_mac, 1234), self_mac), None);
        }

        #[test]
        fn classify_mdns_ignores_non_ip_non_udp_and_short_frames() {
            let self_mac = [0x02, 0, 0, 0, 0, 1];

            // ARP (non-IP)
            let mut arp = vec![
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0x02, 0, 0, 0, 0, 1,
                0x08, 0x06, // ethertype ARP
            ];
            arp.extend_from_slice(&[0u8; 20]);
            assert_eq!(classify_mdns(&arp, self_mac), None);

            // Too short
            assert_eq!(classify_mdns(&[0u8; 5], self_mac), None);
            assert_eq!(classify_mdns(&[0u8; 30], self_mac), None);
        }
    }
}

pub mod os {
    pub mod tun {
        use std::fs::{File, OpenOptions};
        use std::os::fd::AsRawFd;

        #[derive(Debug, PartialEq, Eq)]
        pub enum Error {
            InvalidName,
            Io(i32),
        }

        #[derive(Debug, PartialEq, Eq)]
        pub struct IfReqPlan {
            pub name: [u8; libc::IFNAMSIZ],
            pub flags: libc::c_short,
        }

        pub fn plan_tap_ifreq(name: &str) -> Result<IfReqPlan, Error> {
            let bytes = name.as_bytes();
            if bytes.is_empty() || bytes.len() >= libc::IFNAMSIZ {
                return Err(Error::InvalidName);
            }

            let mut if_name = [0u8; libc::IFNAMSIZ];
            if_name[..bytes.len()].copy_from_slice(bytes);
            Ok(IfReqPlan {
                name: if_name,
                flags: (libc::IFF_TAP | libc::IFF_NO_PI) as libc::c_short,
            })
        }

        pub fn open_tap(name: &str) -> Result<File, Error> {
            let plan = plan_tap_ifreq(name)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/net/tun")
                .map_err(errno_from_io)?;
            let mut request = TunIfReq::from_plan(&plan);

            // SAFETY: request is a C-compatible ifreq prefix accepted by TUNSETIFF;
            // the fd is an open /dev/net/tun file and the kernel writes only within it.
            let rc = unsafe { libc::ioctl(file.as_raw_fd(), libc::TUNSETIFF, &mut request) };
            if rc < 0 {
                return Err(last_errno());
            }

            Ok(file)
        }

        #[repr(C)]
        struct TunIfReq {
            name: [libc::c_char; libc::IFNAMSIZ],
            flags: libc::c_short,
        }

        impl TunIfReq {
            fn from_plan(plan: &IfReqPlan) -> Self {
                let mut name = [0; libc::IFNAMSIZ];
                for (dst, src) in name.iter_mut().zip(plan.name) {
                    *dst = src as libc::c_char;
                }
                Self {
                    name,
                    flags: plan.flags,
                }
            }
        }

        fn errno_from_io(err: std::io::Error) -> Error {
            Error::Io(err.raw_os_error().unwrap_or(libc::EIO))
        }

        fn last_errno() -> Error {
            Error::Io(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn plans_tap_ifreq_for_awdl0_without_packet_info() {
                let plan = plan_tap_ifreq("awdl0").expect("valid tap name");

                assert_eq!(&plan.name[..5], b"awdl0");
                assert_eq!(plan.name[5], 0);
                assert_eq!(
                    plan.flags,
                    (libc::IFF_TAP | libc::IFF_NO_PI) as libc::c_short
                );
            }
        }
    }

    pub mod packet {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

        #[derive(Debug, PartialEq, Eq)]
        pub struct BindPlan {
            pub ifindex: libc::c_int,
            pub protocol_be: u16,
        }

        pub fn plan_bind(ifindex: libc::c_int) -> Result<BindPlan, Error> {
            if ifindex <= 0 {
                return Err(Error::InvalidIfIndex);
            }
            Ok(BindPlan {
                ifindex,
                protocol_be: (libc::ETH_P_ALL as u16).to_be(),
            })
        }

        #[derive(Debug, PartialEq, Eq)]
        pub enum Error {
            InvalidIfIndex,
            InvalidName,
            Io(i32),
        }

        pub struct PacketSocket {
            fd: OwnedFd,
        }

        impl PacketSocket {
            pub fn open_bound(iface: &str) -> Result<Self, Error> {
                let iface = CString::new(iface).map_err(|_| Error::InvalidName)?;
                // SAFETY: iface is a valid NUL-terminated C string.
                let ifindex = unsafe { libc::if_nametoindex(iface.as_ptr()) };
                if ifindex == 0 {
                    return Err(last_errno());
                }
                Self::open_bound_ifindex(ifindex as libc::c_int)
            }

            pub fn open_bound_ifindex(ifindex: libc::c_int) -> Result<Self, Error> {
                let plan = plan_bind(ifindex)?;
                // The protocol arg is a __be16 (htons), not a byte-swapped
                // 32-bit int: libc::ETH_P_ALL.to_be() would yield 0x0300_0000,
                // truncated by the kernel to protocol 0. Use the htons-of-u16
                // value (same as the bind below).
                // SAFETY: socket arguments are constants for an AF_PACKET raw socket.
                let fd = unsafe {
                    libc::socket(
                        libc::AF_PACKET,
                        libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                        libc::c_int::from(plan.protocol_be),
                    )
                };
                if fd < 0 {
                    return Err(last_errno());
                }
                // SAFETY: fd was just returned by socket and is uniquely owned here.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };

                let addr = libc::sockaddr_ll {
                    sll_family: libc::AF_PACKET as libc::c_ushort,
                    sll_protocol: plan.protocol_be,
                    sll_ifindex: plan.ifindex,
                    sll_hatype: 0,
                    sll_pkttype: 0,
                    sll_halen: 0,
                    sll_addr: [0; 8],
                };
                // SAFETY: addr points to a valid sockaddr_ll for the duration of the call.
                let rc = unsafe {
                    libc::bind(
                        fd.as_raw_fd(),
                        (&addr as *const libc::sockaddr_ll).cast::<libc::sockaddr>(),
                        std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
                    )
                };
                if rc < 0 {
                    return Err(last_errno());
                }

                Ok(Self { fd })
            }

            pub fn recv(&self, buf: &mut [u8]) -> Result<usize, Error> {
                // SAFETY: buf is valid writable memory for buf.len() bytes.
                let rc = unsafe {
                    libc::recv(
                        self.fd.as_raw_fd(),
                        buf.as_mut_ptr().cast::<libc::c_void>(),
                        buf.len(),
                        0,
                    )
                };
                if rc < 0 {
                    return Err(last_errno());
                }
                Ok(rc as usize)
            }

            pub fn send(&self, frame: &[u8]) -> Result<usize, Error> {
                // SAFETY: frame is valid readable memory for frame.len() bytes.
                let rc = unsafe {
                    libc::send(
                        self.fd.as_raw_fd(),
                        frame.as_ptr().cast::<libc::c_void>(),
                        frame.len(),
                        0,
                    )
                };
                if rc < 0 {
                    return Err(last_errno());
                }
                Ok(rc as usize)
            }

            pub fn as_raw_fd(&self) -> RawFd {
                self.fd.as_raw_fd()
            }
        }

        fn last_errno() -> Error {
            Error::Io(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn plans_packet_bind_for_eth_p_all() {
                let plan = plan_bind(7).expect("valid ifindex");

                assert_eq!(plan.ifindex, 7);
                assert_eq!(plan.protocol_be, (libc::ETH_P_ALL as u16).to_be());
            }
        }
    }

    pub mod netdev {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        #[derive(Debug, PartialEq, Eq)]
        pub enum Error {
            InvalidName,
            Io(i32),
        }

        #[derive(Debug, PartialEq, Eq)]
        pub struct IfName {
            pub bytes: [u8; libc::IFNAMSIZ],
        }

        pub fn plan_if_name(name: &str) -> Result<IfName, Error> {
            let raw = name.as_bytes();
            if raw.is_empty() || raw.len() >= libc::IFNAMSIZ {
                return Err(Error::InvalidName);
            }
            let mut bytes = [0u8; libc::IFNAMSIZ];
            bytes[..raw.len()].copy_from_slice(raw);
            Ok(IfName { bytes })
        }

        pub fn up_flags(existing: libc::c_short) -> libc::c_short {
            existing | (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short
        }

        pub fn down_flags(existing: libc::c_short) -> libc::c_short {
            existing & !(libc::IFF_UP as libc::c_short)
        }

        pub fn get_hwaddr(iface: &str) -> Result<[u8; 6], Error> {
            let socket = control_socket()?;
            let mut request = IfReqHwAddr::new(iface)?;
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCGIFHWADDR.
            let rc = unsafe {
                libc::ioctl(
                    socket.as_raw_fd(),
                    libc::SIOCGIFHWADDR,
                    &mut request as *mut IfReqHwAddr,
                )
            };
            if rc < 0 {
                return Err(last_errno());
            }
            Ok(request.hwaddr())
        }

        pub fn set_hwaddr(iface: &str, hwaddr: [u8; 6]) -> Result<(), Error> {
            let socket = control_socket()?;
            let mut request = IfReqHwAddr::new(iface)?;
            request.set_hwaddr(hwaddr);
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCSIFHWADDR.
            let rc = unsafe {
                libc::ioctl(
                    socket.as_raw_fd(),
                    libc::SIOCSIFHWADDR,
                    &mut request as *mut IfReqHwAddr,
                )
            };
            if rc < 0 {
                return Err(last_errno());
            }
            Ok(())
        }

        pub fn set_mtu(iface: &str, mtu: libc::c_int) -> Result<(), Error> {
            let socket = control_socket()?;
            let mut request = IfReqMtu::new(iface, mtu)?;
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCSIFMTU.
            let rc = unsafe {
                libc::ioctl(
                    socket.as_raw_fd(),
                    libc::SIOCSIFMTU,
                    &mut request as *mut IfReqMtu,
                )
            };
            if rc < 0 {
                return Err(last_errno());
            }
            Ok(())
        }

        pub fn set_up(iface: &str) -> Result<(), Error> {
            let socket = control_socket()?;
            let mut request = IfReqFlags::new(iface, 0)?;
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCGIFFLAGS.
            let rc = unsafe {
                libc::ioctl(
                    socket.as_raw_fd(),
                    libc::SIOCGIFFLAGS,
                    &mut request as *mut IfReqFlags,
                )
            };
            if rc < 0 {
                return Err(last_errno());
            }
            request.flags = up_flags(request.flags);
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCSIFFLAGS.
            let rc = unsafe {
                libc::ioctl(
                    socket.as_raw_fd(),
                    libc::SIOCSIFFLAGS,
                    &mut request as *mut IfReqFlags,
                )
            };
            if rc < 0 {
                return Err(last_errno());
            }
            Ok(())
        }

        /// Current kernel ifindex for `iface`, or 0 if it doesn't exist. A
        /// USB unplug/replug re-enumerates the device → the netdev is destroyed
        /// and recreated with a NEW ifindex (even though udev gives it the same
        /// name), which silently invalidates any AF_PACKET socket bound to the
        /// old index. Comparing the ifindex detects that "same name, new
        /// device" case that IFF_UP cannot.
        pub fn ifindex(iface: &str) -> u32 {
            match std::ffi::CString::new(iface) {
                // SAFETY: cs is a valid NUL-terminated C string.
                Ok(cs) => unsafe { libc::if_nametoindex(cs.as_ptr()) },
                Err(_) => 0,
            }
        }

        /// True if the interface exists and is administratively UP (IFF_UP).
        /// Returns false on any error (missing iface, ioctl failure) so callers
        /// can treat "can't tell / gone" the same as "down" and recover.
        pub fn is_up(iface: &str) -> bool {
            let socket = match control_socket() {
                Ok(s) => s,
                Err(_) => return false,
            };
            let mut request = match IfReqFlags::new(iface, 0) {
                Ok(r) => r,
                Err(_) => return false,
            };
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCGIFFLAGS.
            let rc = unsafe {
                libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS, &mut request as *mut IfReqFlags)
            };
            if rc < 0 {
                return false;
            }
            request.flags & (libc::IFF_UP as libc::c_short) != 0
        }

        /// Bring an interface administratively DOWN (clears IFF_UP). Needed
        /// before changing the interface type (nl80211 rejects a type change
        /// on an up interface).
        pub fn set_down(iface: &str) -> Result<(), Error> {
            let socket = control_socket()?;
            let mut request = IfReqFlags::new(iface, 0)?;
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCGIFFLAGS.
            let rc = unsafe {
                libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS, &mut request as *mut IfReqFlags)
            };
            if rc < 0 {
                return Err(last_errno());
            }
            request.flags = down_flags(request.flags);
            // SAFETY: request is a valid ifreq-compatible buffer for SIOCSIFFLAGS.
            let rc = unsafe {
                libc::ioctl(socket.as_raw_fd(), libc::SIOCSIFFLAGS, &mut request as *mut IfReqFlags)
            };
            if rc < 0 {
                return Err(last_errno());
            }
            Ok(())
        }

        fn control_socket() -> Result<OwnedFd, Error> {
            // SAFETY: socket arguments open a datagram IPv4 control socket for ioctls.
            let fd =
                unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
            if fd < 0 {
                return Err(last_errno());
            }
            // SAFETY: fd was just returned by socket and is uniquely owned here.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }

        #[repr(C)]
        struct IfReqFlags {
            name: [libc::c_char; libc::IFNAMSIZ],
            flags: libc::c_short,
        }

        #[repr(C)]
        struct IfReqMtu {
            name: [libc::c_char; libc::IFNAMSIZ],
            mtu: libc::c_int,
        }

        #[repr(C)]
        struct IfReqHwAddr {
            name: [libc::c_char; libc::IFNAMSIZ],
            addr: libc::sockaddr,
        }

        impl IfReqFlags {
            fn new(iface: &str, flags: libc::c_short) -> Result<Self, Error> {
                Ok(Self {
                    name: c_name(iface)?,
                    flags,
                })
            }
        }

        impl IfReqMtu {
            fn new(iface: &str, mtu: libc::c_int) -> Result<Self, Error> {
                Ok(Self {
                    name: c_name(iface)?,
                    mtu,
                })
            }
        }

        impl IfReqHwAddr {
            fn new(iface: &str) -> Result<Self, Error> {
                Ok(Self {
                    name: c_name(iface)?,
                    addr: libc::sockaddr {
                        sa_family: libc::ARPHRD_ETHER as libc::sa_family_t,
                        sa_data: [0; 14],
                    },
                })
            }

            fn set_hwaddr(&mut self, hwaddr: [u8; 6]) {
                self.addr.sa_family = libc::ARPHRD_ETHER as libc::sa_family_t;
                for (dst, src) in self.addr.sa_data.iter_mut().zip(hwaddr) {
                    *dst = src as libc::c_char;
                }
            }

            fn hwaddr(&self) -> [u8; 6] {
                let mut hwaddr = [0u8; 6];
                for (dst, src) in hwaddr.iter_mut().zip(self.addr.sa_data) {
                    *dst = src as u8;
                }
                hwaddr
            }
        }

        fn c_name(name: &str) -> Result<[libc::c_char; libc::IFNAMSIZ], Error> {
            let planned = plan_if_name(name)?;
            let mut out = [0; libc::IFNAMSIZ];
            for (dst, src) in out.iter_mut().zip(planned.bytes) {
                *dst = src as libc::c_char;
            }
            Ok(out)
        }

        fn last_errno() -> Error {
            Error::Io(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn plans_if_name_and_up_flags() {
                let name = plan_if_name("awdl0").expect("valid if name");

                assert_eq!(&name.bytes[..5], b"awdl0");
                assert_eq!(name.bytes[5], 0);
                assert_eq!(
                    up_flags(0),
                    (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short
                );
            }
        }
    }

    pub mod nl80211 {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        const GENL_ID_CTRL: u16 = 0x10;
        const CTRL_CMD_GETFAMILY: u8 = 3;
        const CTRL_ATTR_FAMILY_ID: u16 = 1;
        const CTRL_ATTR_FAMILY_NAME: u16 = 2;
        const NL80211_CMD_SET_CHANNEL: u8 = 65;
        const NL80211_ATTR_IFINDEX: u16 = 3;
        const NL80211_ATTR_WIPHY_FREQ: u16 = 38;

        #[derive(Debug, PartialEq, Eq)]
        pub struct ChannelRequest {
            pub ifindex: libc::c_int,
            pub frequency_mhz: u32,
        }

        pub fn plan_set_channel(
            ifindex: libc::c_int,
            frequency_mhz: u32,
        ) -> Result<ChannelRequest, Error> {
            if ifindex <= 0 || frequency_mhz == 0 {
                return Err(Error::Invalid);
            }
            Ok(ChannelRequest {
                ifindex,
                frequency_mhz,
            })
        }

        #[derive(Debug, PartialEq, Eq)]
        pub enum Error {
            Invalid,
            InvalidName,
            Io(i32),
            Protocol,
        }

        pub fn set_channel(iface: &str, frequency_mhz: u32) -> Result<(), Error> {
            let iface = CString::new(iface).map_err(|_| Error::InvalidName)?;
            // SAFETY: iface is a valid NUL-terminated C string.
            let ifindex = unsafe { libc::if_nametoindex(iface.as_ptr()) };
            if ifindex == 0 {
                return Err(last_errno());
            }
            set_channel_ifindex(ifindex as libc::c_int, frequency_mhz)
        }

        const NL80211_CMD_GET_WIPHY: u8 = 1;
        // Nested attribute listing the iftypes the wiphy's driver supports.
        const NL80211_ATTR_SUPPORTED_IFTYPES: u16 = 32;
        // Netlink attribute `type` flag bits (nested / byte-order); the low 14
        // bits are the actual id. Mask them off before comparing/decoding.
        const NLA_TYPE_MASK: u16 = 0x3fff;

        /// Human-readable name for an `nl80211_iftype` id — used in the adapter
        /// capability messages. Unknown ids render as `iftype<N>`.
        pub fn iftype_name(iftype: u32) -> std::borrow::Cow<'static, str> {
            use std::borrow::Cow;
            match iftype {
                1 => Cow::Borrowed("ibss"),
                2 => Cow::Borrowed("managed"),
                3 => Cow::Borrowed("ap"),
                4 => Cow::Borrowed("ap-vlan"),
                5 => Cow::Borrowed("wds"),
                6 => Cow::Borrowed("monitor"),
                7 => Cow::Borrowed("mesh"),
                8 => Cow::Borrowed("p2p-client"),
                9 => Cow::Borrowed("p2p-go"),
                10 => Cow::Borrowed("p2p-device"),
                11 => Cow::Borrowed("ocb"),
                12 => Cow::Borrowed("nan"),
                other => Cow::Owned(format!("iftype{other}")),
            }
        }

        /// Query the wiphy backing `iface` for the interface modes its driver
        /// supports (`GET_WIPHY` → `NL80211_ATTR_SUPPORTED_IFTYPES`). Read-only
        /// and works without root or bringing the interface up — used to
        /// explain *why* an adapter can't serve as filin's monitor radio (e.g.
        /// it advertises no `monitor` mode at all).
        pub fn supported_iftypes(iface: &str) -> Result<Vec<u32>, Error> {
            let cs = CString::new(iface).map_err(|_| Error::InvalidName)?;
            // SAFETY: cs is a valid NUL-terminated C string.
            let ifindex = unsafe { libc::if_nametoindex(cs.as_ptr()) };
            if ifindex == 0 {
                return Err(last_errno());
            }
            let socket = NetlinkSocket::open()?;
            let family_id = socket.resolve_family("nl80211")?;
            let mut payload = vec![NL80211_CMD_GET_WIPHY, 1, 0, 0];
            push_attr_u32(&mut payload, NL80211_ATTR_IFINDEX, ifindex);
            let response =
                socket.request_big(family_id, libc::NLM_F_REQUEST as u16, &payload)?;
            Ok(parse_supported_iftypes(&response))
        }

        /// True iff `iface`'s driver lists `monitor` among its supported modes.
        pub fn supports_monitor(iface: &str) -> Result<bool, Error> {
            Ok(supported_iftypes(iface)?.contains(&NL80211_IFTYPE_MONITOR))
        }

        /// Parse a `GET_WIPHY` genl payload and return the iftype ids carried in
        /// the (nested) `NL80211_ATTR_SUPPORTED_IFTYPES` attribute. In that
        /// nest each sub-attribute's *type* field IS the iftype id and the
        /// value is empty. Pure (no I/O) so it is unit-testable; returns an
        /// empty vec if the attribute is absent or the buffer is malformed.
        fn parse_supported_iftypes(payload: &[u8]) -> Vec<u32> {
            // Skip the 4-byte genlmsghdr (cmd, version, reserved); attrs follow.
            if payload.len() < 4 {
                return Vec::new();
            }
            let mut attrs = &payload[4..];
            while attrs.len() >= 4 {
                let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
                let kind = u16::from_ne_bytes([attrs[2], attrs[3]]) & NLA_TYPE_MASK;
                if len < 4 || len > attrs.len() {
                    break;
                }
                if kind == NL80211_ATTR_SUPPORTED_IFTYPES {
                    return parse_iftype_nest(&attrs[4..len]);
                }
                let aligned = align4(len);
                if aligned == 0 || aligned > attrs.len() {
                    break;
                }
                attrs = &attrs[aligned..];
            }
            Vec::new()
        }

        fn parse_iftype_nest(nest: &[u8]) -> Vec<u32> {
            let mut out = Vec::new();
            let mut p = nest;
            while p.len() >= 4 {
                let len = u16::from_ne_bytes([p[0], p[1]]) as usize;
                let kind = u16::from_ne_bytes([p[2], p[3]]) & NLA_TYPE_MASK;
                if len < 4 {
                    break;
                }
                out.push(u32::from(kind));
                let aligned = align4(len);
                if aligned == 0 || aligned > p.len() {
                    break;
                }
                p = &p[aligned..];
            }
            out
        }

        const NL80211_CMD_SET_INTERFACE: u8 = 6;
        const NL80211_ATTR_IFTYPE: u16 = 5;
        const NL80211_IFTYPE_MONITOR: u32 = 6;

        /// Set an interface to monitor mode (nl80211 SET_INTERFACE,
        /// iftype=MONITOR). The interface must be DOWN first (see
        /// `netdev::set_down`). Recovers a card that re-enumerated in managed
        /// mode after a USB bounce.
        pub fn set_monitor(iface: &str) -> Result<(), Error> {
            let cs = CString::new(iface).map_err(|_| Error::InvalidName)?;
            // SAFETY: cs is a valid NUL-terminated C string.
            let ifindex = unsafe { libc::if_nametoindex(cs.as_ptr()) };
            if ifindex == 0 {
                return Err(last_errno());
            }
            let socket = NetlinkSocket::open()?;
            let family_id = socket.resolve_family("nl80211")?;
            let mut payload = vec![NL80211_CMD_SET_INTERFACE, 1, 0, 0];
            push_attr_u32(&mut payload, NL80211_ATTR_IFINDEX, ifindex);
            push_attr_u32(&mut payload, NL80211_ATTR_IFTYPE, NL80211_IFTYPE_MONITOR);
            socket.request(
                family_id,
                libc::NLM_F_REQUEST as u16 | libc::NLM_F_ACK as u16,
                &payload,
            )?;
            Ok(())
        }

        pub fn set_channel_ifindex(ifindex: libc::c_int, frequency_mhz: u32) -> Result<(), Error> {
            let request = plan_set_channel(ifindex, frequency_mhz)?;
            let socket = NetlinkSocket::open()?;
            let family_id = socket.resolve_family("nl80211")?;
            let mut payload = vec![NL80211_CMD_SET_CHANNEL, 1, 0, 0];
            push_attr_u32(&mut payload, NL80211_ATTR_IFINDEX, request.ifindex as u32);
            push_attr_u32(&mut payload, NL80211_ATTR_WIPHY_FREQ, request.frequency_mhz);
            socket.request(
                family_id,
                libc::NLM_F_REQUEST as u16 | libc::NLM_F_ACK as u16,
                &payload,
            )?;
            Ok(())
        }

        struct NetlinkSocket {
            fd: OwnedFd,
            seq: std::cell::Cell<u32>,
        }

        impl NetlinkSocket {
            fn open() -> Result<Self, Error> {
                // SAFETY: socket arguments open a generic netlink socket.
                let fd = unsafe {
                    libc::socket(
                        libc::AF_NETLINK,
                        libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                        libc::NETLINK_GENERIC,
                    )
                };
                if fd < 0 {
                    return Err(last_errno());
                }
                // SAFETY: fd was just returned by socket and is uniquely owned here.
                Ok(Self {
                    fd: unsafe { OwnedFd::from_raw_fd(fd) },
                    seq: std::cell::Cell::new(1),
                })
            }

            fn resolve_family(&self, family: &str) -> Result<u16, Error> {
                let mut payload = vec![CTRL_CMD_GETFAMILY, 1, 0, 0];
                push_attr_bytes(&mut payload, CTRL_ATTR_FAMILY_NAME, family.as_bytes(), true);
                let response = self.request(GENL_ID_CTRL, libc::NLM_F_REQUEST as u16, &payload)?;
                parse_family_id(&response).ok_or(Error::Protocol)
            }

            fn request(
                &self,
                nlmsg_type: u16,
                flags: u16,
                payload: &[u8],
            ) -> Result<Vec<u8>, Error> {
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1));
                let mut msg = Vec::with_capacity(16 + payload.len());
                msg.extend_from_slice(&((16 + payload.len()) as u32).to_ne_bytes());
                msg.extend_from_slice(&nlmsg_type.to_ne_bytes());
                msg.extend_from_slice(&flags.to_ne_bytes());
                msg.extend_from_slice(&seq.to_ne_bytes());
                msg.extend_from_slice(&0u32.to_ne_bytes());
                msg.extend_from_slice(payload);

                // SAFETY: msg is valid readable memory for msg.len() bytes.
                let sent = unsafe {
                    libc::send(
                        self.fd.as_raw_fd(),
                        msg.as_ptr().cast::<libc::c_void>(),
                        msg.len(),
                        0,
                    )
                };
                if sent < 0 {
                    return Err(last_errno());
                }

                let mut buf = vec![0u8; 8192];
                // SAFETY: buf is valid writable memory for buf.len() bytes.
                let len = unsafe {
                    libc::recv(
                        self.fd.as_raw_fd(),
                        buf.as_mut_ptr().cast::<libc::c_void>(),
                        buf.len(),
                        0,
                    )
                };
                if len < 0 {
                    return Err(last_errno());
                }
                buf.truncate(len as usize);
                parse_ack_or_payload(&buf)
            }

            /// Like [`request`], but with a large receive buffer. A non-dump
            /// `GET_WIPHY` reply carries the whole phy description in a single
            /// netlink message that routinely exceeds 8 KiB, so the default
            /// buffer would truncate it (and drop the iftype list we want).
            fn request_big(
                &self,
                nlmsg_type: u16,
                flags: u16,
                payload: &[u8],
            ) -> Result<Vec<u8>, Error> {
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1));
                let mut msg = Vec::with_capacity(16 + payload.len());
                msg.extend_from_slice(&((16 + payload.len()) as u32).to_ne_bytes());
                msg.extend_from_slice(&nlmsg_type.to_ne_bytes());
                msg.extend_from_slice(&flags.to_ne_bytes());
                msg.extend_from_slice(&seq.to_ne_bytes());
                msg.extend_from_slice(&0u32.to_ne_bytes());
                msg.extend_from_slice(payload);

                // SAFETY: msg is valid readable memory for msg.len() bytes.
                let sent = unsafe {
                    libc::send(
                        self.fd.as_raw_fd(),
                        msg.as_ptr().cast::<libc::c_void>(),
                        msg.len(),
                        0,
                    )
                };
                if sent < 0 {
                    return Err(last_errno());
                }

                let mut buf = vec![0u8; 65536];
                // SAFETY: buf is valid writable memory for buf.len() bytes.
                let len = unsafe {
                    libc::recv(
                        self.fd.as_raw_fd(),
                        buf.as_mut_ptr().cast::<libc::c_void>(),
                        buf.len(),
                        0,
                    )
                };
                if len < 0 {
                    return Err(last_errno());
                }
                buf.truncate(len as usize);
                parse_ack_or_payload(&buf)
            }
        }

        fn parse_ack_or_payload(buf: &[u8]) -> Result<Vec<u8>, Error> {
            if buf.len() < 16 {
                return Err(Error::Protocol);
            }
            let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            let kind = u16::from_ne_bytes([buf[4], buf[5]]);
            if len > buf.len() || len < 16 {
                return Err(Error::Protocol);
            }
            if kind == libc::NLMSG_ERROR as u16 {
                if len < 20 {
                    return Err(Error::Protocol);
                }
                let code = i32::from_ne_bytes([buf[16], buf[17], buf[18], buf[19]]);
                return if code == 0 {
                    Ok(Vec::new())
                } else {
                    Err(Error::Io(-code))
                };
            }
            Ok(buf[16..len].to_vec())
        }

        fn parse_family_id(payload: &[u8]) -> Option<u16> {
            if payload.len() < 4 {
                return None;
            }
            let mut attrs = &payload[4..];
            while attrs.len() >= 4 {
                let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
                let kind = u16::from_ne_bytes([attrs[2], attrs[3]]);
                if len < 4 || len > attrs.len() {
                    return None;
                }
                if kind == CTRL_ATTR_FAMILY_ID && len >= 6 {
                    return Some(u16::from_ne_bytes([attrs[4], attrs[5]]));
                }
                let aligned = align4(len);
                if aligned > attrs.len() {
                    return None;
                }
                attrs = &attrs[aligned..];
            }
            None
        }

        fn push_attr_u32(buf: &mut Vec<u8>, kind: u16, value: u32) {
            push_attr_bytes(buf, kind, &value.to_ne_bytes(), false);
        }

        fn push_attr_bytes(buf: &mut Vec<u8>, kind: u16, value: &[u8], nul: bool) {
            let payload_len = value.len() + usize::from(nul);
            let len = 4 + payload_len;
            buf.extend_from_slice(&(len as u16).to_ne_bytes());
            buf.extend_from_slice(&kind.to_ne_bytes());
            buf.extend_from_slice(value);
            if nul {
                buf.push(0);
            }
            while !buf.len().is_multiple_of(4) {
                buf.push(0);
            }
        }

        fn align4(len: usize) -> usize {
            (len + 3) & !3
        }

        fn last_errno() -> Error {
            Error::Io(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn plans_set_channel_request() {
                let request = plan_set_channel(9, 5220).expect("valid request");

                assert_eq!(request.ifindex, 9);
                assert_eq!(request.frequency_mhz, 5220);
            }

            /// Append a netlink attribute (header + value, padded to 4 bytes).
            fn push_attr(buf: &mut Vec<u8>, kind: u16, value: &[u8]) {
                let len = 4 + value.len();
                buf.extend_from_slice(&(len as u16).to_ne_bytes());
                buf.extend_from_slice(&kind.to_ne_bytes());
                buf.extend_from_slice(value);
                while !buf.len().is_multiple_of(4) {
                    buf.push(0);
                }
            }

            /// Build the nested SUPPORTED_IFTYPES attribute body: one empty
            /// sub-attr per iftype, whose `type` field is the iftype id.
            fn iftype_nest(iftypes: &[u16]) -> Vec<u8> {
                let mut nest = Vec::new();
                for &t in iftypes {
                    push_attr(&mut nest, t, &[]);
                }
                nest
            }

            #[test]
            fn parses_supported_iftypes_from_wiphy_payload() {
                // genlmsghdr (4 bytes) + a decoy attr + the nested iftypes attr.
                let mut payload = vec![NL80211_CMD_GET_WIPHY, 1, 0, 0];
                push_attr(&mut payload, 2 /* WIPHY_NAME */, b"phy0");
                let nest = iftype_nest(&[2 /* managed */, 6 /* monitor */, 3 /* ap */]);
                // Kernel sets the NESTED flag on this attr's type; we must mask it.
                push_attr(&mut payload, 0x8000 | NL80211_ATTR_SUPPORTED_IFTYPES, &nest);

                let types = parse_supported_iftypes(&payload);
                assert_eq!(types, vec![2, 6, 3]);
                assert!(types.contains(&NL80211_IFTYPE_MONITOR));
            }

            #[test]
            fn parses_iftypes_without_monitor() {
                let mut payload = vec![NL80211_CMD_GET_WIPHY, 1, 0, 0];
                let nest = iftype_nest(&[2 /* managed */, 3 /* ap */]);
                push_attr(&mut payload, NL80211_ATTR_SUPPORTED_IFTYPES, &nest);

                let types = parse_supported_iftypes(&payload);
                assert_eq!(types, vec![2, 3]);
                assert!(!types.contains(&NL80211_IFTYPE_MONITOR));
            }

            #[test]
            fn missing_iftypes_attr_yields_empty() {
                let mut payload = vec![NL80211_CMD_GET_WIPHY, 1, 0, 0];
                push_attr(&mut payload, 2 /* WIPHY_NAME */, b"phy0");
                assert!(parse_supported_iftypes(&payload).is_empty());
            }

            #[test]
            fn truncated_payload_does_not_panic() {
                // A bogus attr len longer than the buffer must terminate cleanly.
                let payload = vec![NL80211_CMD_GET_WIPHY, 1, 0, 0, 0xff, 0xff, 0x20, 0x00];
                assert!(parse_supported_iftypes(&payload).is_empty());
            }

            #[test]
            fn iftype_names_are_human_readable() {
                assert_eq!(iftype_name(2), "managed");
                assert_eq!(iftype_name(6), "monitor");
                assert_eq!(iftype_name(99), "iftype99");
            }
        }
    }

    /// rtnetlink neighbor-table manipulation (FILIN_NEIGHBOR_TABLE.md).
    /// AWDL does not use NDP; filin must install static IPv6→MAC mappings via
    /// `RTM_NEWNEIGH` (NUD_PERMANENT) so unicast awdl0 connections work —
    /// mirrors owl's `neighbor_add_rfc4291` (owl/daemon/netutils.c:595).
    pub mod rtnl {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        // rtnetlink message types (uclibc/libc RTM_* constants).
        const RTM_NEWNEIGH: u16 = 28;
        const RTM_DELNEIGH: u16 = 29;
        // rtm attribute ids (linux/include/uapi/linux/neighbour.h).
        const NDA_DST: u16 = 1;
        const NDA_LLADDR: u16 = 2;
        // NUD_PERMANENT = 0x80 (linux/include/uapi/linux/neighbour.h).
        const NUD_PERMANENT: u16 = 0x80;
        // AF_INET6 = 10.
        const AF_INET6: u8 = 10;

        #[derive(Debug, PartialEq, Eq)]
        pub struct NeighborRequest {
            pub ifindex: libc::c_int,
            pub ipv6: [u8; 16],
            pub mac: [u8; 6],
        }

        #[derive(Debug, PartialEq, Eq)]
        pub enum Error {
            Invalid,
            InvalidName,
            Io(i32),
            Protocol,
        }

        /// Validate a neighbor add/remove request. Mirrors `plan_set_channel`.
        pub fn plan_neighbor(
            ifindex: libc::c_int,
            ipv6: [u8; 16],
            mac: [u8; 6],
        ) -> Result<NeighborRequest, Error> {
            if ifindex <= 0 {
                return Err(Error::Invalid);
            }
            // Reject non-link-local (fe80::/10) addresses — only AWDL-derived
            // link-local mappings make sense here.
            if ipv6[0] != 0xfe || (ipv6[1] & 0xc0) != 0x80 {
                return Err(Error::Invalid);
            }
            Ok(NeighborRequest { ifindex, ipv6, mac })
        }

        /// Build the netlink message header flags + payload for an RTM_NEWNEIGH
        /// (add/replace) or RTM_DELNEIGH (remove) request. Pure (no I/O), so
        /// unit-testable. Returns `(msg_type, nlmsg_flags, payload)`; the
        /// payload starts with the `ndmsg` struct (12 bytes) followed by
        /// rtattrs (NDA_DST = 16-byte IPv6, NDA_LLADDR = 6-byte MAC).
        ///
        /// The flags MUST be forwarded to the netlink send: RTM_NEWNEIGH
        /// without NLM_F_CREATE makes the kernel `neigh_add` return -ENOENT for
        /// a not-yet-existing entry, so the static AWDL neighbor would never be
        /// installed.
        pub fn build_neighbor_payload(req: &NeighborRequest, remove: bool) -> (u16, u16, Vec<u8>) {
            let msg_type = if remove { RTM_DELNEIGH } else { RTM_NEWNEIGH };
            let flags = if remove {
                libc::NLM_F_REQUEST as u16 | libc::NLM_F_ACK as u16
            } else {
                libc::NLM_F_REQUEST as u16
                    | libc::NLM_F_ACK as u16
                    | libc::NLM_F_CREATE as u16
                    | libc::NLM_F_REPLACE as u16
            };
            // ndmsg struct (linux/include/uapi/linux/neighbour.h):
            //   u8 ndm_family;  u8 ndm_pad1;  u16 ndm_pad2;
            //   s32 ndm_ifindex;  u16 ndm_state;  u8 ndm_flags;  u8 ndm_type;
            let mut payload = Vec::with_capacity(12 + 24 + 12);
            payload.push(AF_INET6); // ndm_family
            payload.push(0); // ndm_pad1
            payload.extend_from_slice(&0u16.to_ne_bytes()); // ndm_pad2
            payload.extend_from_slice(&req.ifindex.to_ne_bytes()); // ndm_ifindex
            payload.extend_from_slice(&NUD_PERMANENT.to_ne_bytes()); // ndm_state
            payload.push(0); // ndm_flags
            payload.push(0); // ndm_type
            // NDA_DST (IPv6 address, 16 bytes).
            push_attr(&mut payload, NDA_DST, &req.ipv6);
            // NDA_LLADDR (MAC, 6 bytes) — only for NEWNEIGH; DELNEIGH keys on
            // the dst alone but including lladdr is harmless and keeps the
            // builder symmetric.
            push_attr(&mut payload, NDA_LLADDR, &req.mac);
            (msg_type, flags, payload)
        }

        fn push_attr(buf: &mut Vec<u8>, kind: u16, value: &[u8]) {
            let len = 4 + value.len();
            buf.extend_from_slice(&(len as u16).to_ne_bytes());
            buf.extend_from_slice(&kind.to_ne_bytes());
            buf.extend_from_slice(value);
            while !buf.len().is_multiple_of(4) {
                buf.push(0);
            }
        }

        /// Add (or replace) a static neighbor entry: `{ipv6 → mac}` on `iface`.
        /// Idempotent via NLM_F_REPLACE; logs and continues on error (the
        /// daemon stays up — a missing neighbor only affects one peer's
        /// unicast path). Mirrors owl's `neighbor_add_rfc4291`.
        pub fn add_neighbor(iface: &str, ipv6: [u8; 16], mac: [u8; 6]) -> Result<(), Error> {
            let ifindex = resolve_ifindex(iface)?;
            add_neighbor_ifindex(ifindex, ipv6, mac)
        }

        pub fn add_neighbor_ifindex(
            ifindex: libc::c_int,
            ipv6: [u8; 16],
            mac: [u8; 6],
        ) -> Result<(), Error> {
            let req = plan_neighbor(ifindex, ipv6, mac)?;
            let (msg_type, flags, payload) = build_neighbor_payload(&req, false);
            send_rtnl(msg_type, flags, &payload)
        }

        /// Remove the static neighbor entry for `ipv6` on `iface`.
        pub fn remove_neighbor(iface: &str, ipv6: [u8; 16]) -> Result<(), Error> {
            let ifindex = resolve_ifindex(iface)?;
            remove_neighbor_ifindex(ifindex, ipv6)
        }

        pub fn remove_neighbor_ifindex(ifindex: libc::c_int, ipv6: [u8; 16]) -> Result<(), Error> {
            // DELNEIGH keys on (ifindex, dst) — the mac is not required, but
            // build_neighbor_payload wants one; pass zeros (ignored on delete).
            let req = plan_neighbor(ifindex, ipv6, [0; 6])?;
            let (msg_type, flags, payload) = build_neighbor_payload(&req, true);
            send_rtnl(msg_type, flags, &payload)
        }

        fn resolve_ifindex(iface: &str) -> Result<libc::c_int, Error> {
            let iface = CString::new(iface).map_err(|_| Error::InvalidName)?;
            // SAFETY: iface is a valid NUL-terminated C string.
            let ifindex = unsafe { libc::if_nametoindex(iface.as_ptr()) };
            if ifindex == 0 {
                return Err(last_errno());
            }
            Ok(ifindex as libc::c_int)
        }

        fn send_rtnl(msg_type: u16, flags: u16, payload: &[u8]) -> Result<(), Error> {
            let socket = RtnlSocket::open()?;
            socket.request(msg_type, flags, payload)?;
            Ok(())
        }

        struct RtnlSocket {
            fd: OwnedFd,
            seq: std::cell::Cell<u32>,
        }

        impl RtnlSocket {
            fn open() -> Result<Self, Error> {
                // SAFETY: socket arguments open a routing-netlink socket.
                let fd = unsafe {
                    libc::socket(
                        libc::AF_NETLINK,
                        libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                        libc::NETLINK_ROUTE,
                    )
                };
                if fd < 0 {
                    return Err(last_errno());
                }
                // SAFETY: fd was just returned by socket and is uniquely owned.
                Ok(Self {
                    fd: unsafe { OwnedFd::from_raw_fd(fd) },
                    seq: std::cell::Cell::new(1),
                })
            }

            fn request(
                &self,
                nlmsg_type: u16,
                flags: u16,
                payload: &[u8],
            ) -> Result<Vec<u8>, Error> {
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1));
                let total = 16 + payload.len();
                let mut msg = Vec::with_capacity(total);
                msg.extend_from_slice(&(total as u32).to_ne_bytes());
                msg.extend_from_slice(&nlmsg_type.to_ne_bytes());
                msg.extend_from_slice(&flags.to_ne_bytes());
                msg.extend_from_slice(&seq.to_ne_bytes());
                msg.extend_from_slice(&0u32.to_ne_bytes());
                msg.extend_from_slice(payload);

                // SAFETY: msg is valid readable memory for msg.len() bytes.
                let sent = unsafe {
                    libc::send(
                        self.fd.as_raw_fd(),
                        msg.as_ptr().cast::<libc::c_void>(),
                        msg.len(),
                        0,
                    )
                };
                if sent < 0 {
                    return Err(last_errno());
                }

                let mut buf = vec![0u8; 8192];
                // SAFETY: buf is valid writable memory for buf.len() bytes.
                let len = unsafe {
                    libc::recv(
                        self.fd.as_raw_fd(),
                        buf.as_mut_ptr().cast::<libc::c_void>(),
                        buf.len(),
                        0,
                    )
                };
                if len < 0 {
                    return Err(last_errno());
                }
                buf.truncate(len as usize);
                parse_ack(&buf)
            }
        }

        fn parse_ack(buf: &[u8]) -> Result<Vec<u8>, Error> {
            if buf.len() < 16 {
                return Err(Error::Protocol);
            }
            let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            let kind = u16::from_ne_bytes([buf[4], buf[5]]);
            if len > buf.len() || len < 16 {
                return Err(Error::Protocol);
            }
            if kind == libc::NLMSG_ERROR as u16 {
                if len < 20 {
                    return Err(Error::Protocol);
                }
                let code = i32::from_ne_bytes([buf[16], buf[17], buf[18], buf[19]]);
                return if code == 0 {
                    Ok(Vec::new())
                } else {
                    Err(Error::Io(-code))
                };
            }
            Ok(buf[16..len].to_vec())
        }

        fn last_errno() -> Error {
            Error::Io(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn plan_rejects_zero_ifindex() {
                assert_eq!(
                    plan_neighbor(0, [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0xff, 0xfe, 0, 0, 1], [0x02, 0, 0, 0, 0, 1]),
                    Err(Error::Invalid)
                );
            }

            #[test]
            fn plan_rejects_non_link_local_ipv6() {
                assert_eq!(
                    plan_neighbor(5, [0x20, 0x01, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0xff, 0xfe, 0, 0, 1], [0x02, 0, 0, 0, 0, 1]),
                    Err(Error::Invalid)
                );
            }

            #[test]
            fn plan_accepts_link_local_mapping() {
                let ipv6 = [
                    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0xaa, 0xbb, 0xff, 0xfe, 0xcc, 0xdd, 0xee,
                ];
                let req = plan_neighbor(7, ipv6, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee])
                    .expect("valid neighbor request");
                assert_eq!(req.ifindex, 7);
                assert_eq!(req.ipv6, ipv6);
            }

            #[test]
            fn build_payload_add_encodes_ndmsg_with_nud_permanent() {
                let ipv6 = [
                    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0xaa, 0xbb, 0xff, 0xfe, 0xcc, 0xdd, 0xee,
                ];
                let req = plan_neighbor(7, ipv6, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]).unwrap();
                let (msg_type, _flags, payload) = build_neighbor_payload(&req, false);

                assert_eq!(msg_type, RTM_NEWNEIGH);
                // ndmsg: family=AF_INET6, pad1=0, pad2=0, ifindex=7, state=NUD_PERMANENT
                assert_eq!(payload[0], AF_INET6);
                assert_eq!(payload[1], 0);
                assert_eq!(u16::from_ne_bytes([payload[2], payload[3]]), 0);
                assert_eq!(i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]), 7);
                assert_eq!(
                    u16::from_ne_bytes([payload[8], payload[9]]),
                    NUD_PERMANENT
                );
            }

            #[test]
            fn build_payload_delete_uses_delneigh_message_type() {
                let ipv6 = [
                    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0xaa, 0xbb, 0xff, 0xfe, 0xcc, 0xdd, 0xee,
                ];
                let req = plan_neighbor(7, ipv6, [0; 6]).unwrap();
                let (msg_type, _flags, _) = build_neighbor_payload(&req, true);
                assert_eq!(msg_type, RTM_DELNEIGH);
            }

            #[test]
            fn add_request_sets_create_and_replace_flags() {
                // RTM_NEWNEIGH without NLM_F_CREATE makes the kernel reject a
                // not-yet-existing entry with -ENOENT, so the static AWDL
                // neighbor would never be installed. The builder must return
                // CREATE|REPLACE (and the sender must forward them).
                let ipv6 = [
                    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0xaa, 0xbb, 0xff, 0xfe, 0xcc, 0xdd, 0xee,
                ];
                let req = plan_neighbor(7, ipv6, [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]).unwrap();
                let (_msg_type, flags, _payload) = build_neighbor_payload(&req, false);
                assert_ne!(flags & libc::NLM_F_CREATE as u16, 0, "NLM_F_CREATE must be set");
                assert_ne!(flags & libc::NLM_F_REPLACE as u16, 0, "NLM_F_REPLACE must be set");
                assert_ne!(flags & libc::NLM_F_ACK as u16, 0, "NLM_F_ACK must be set");

                // Delete must NOT carry CREATE/REPLACE.
                let dreq = plan_neighbor(7, ipv6, [0; 6]).unwrap();
                let (_t, dflags, _p) = build_neighbor_payload(&dreq, true);
                assert_eq!(dflags & libc::NLM_F_CREATE as u16, 0);
                assert_eq!(dflags & libc::NLM_F_REPLACE as u16, 0);
            }

            #[test]
            fn build_payload_includes_dst_and_lladdr_attrs() {
                let ipv6 = [
                    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0xaa, 0xbb, 0xff, 0xfe, 0xcc, 0xdd, 0xee,
                ];
                let mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
                let req = plan_neighbor(7, ipv6, mac).unwrap();
                let (_msg_type, _flags, payload) = build_neighbor_payload(&req, false);

                // The NDA_DST attr (kind=1) must contain the 16-byte IPv6.
                let dst = find_attr(&payload, NDA_DST).expect("NDA_DST present");
                assert_eq!(dst, &ipv6);
                // The NDA_LLADDR attr (kind=2) must contain the 6-byte MAC.
                let lladdr = find_attr(&payload, NDA_LLADDR).expect("NDA_LLADDR present");
                assert_eq!(&lladdr[..6], &mac);
            }

            /// Scan the rtattrs in a payload (after the 12-byte ndmsg) and
            /// return the value slice for the first attr of `kind`.
            fn find_attr(payload: &[u8], kind: u16) -> Option<&[u8]> {
                let mut attrs = &payload[12..];
                while attrs.len() >= 4 {
                    let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
                    let k = u16::from_ne_bytes([attrs[2], attrs[3]]);
                    if len < 4 || len > attrs.len() {
                        return None;
                    }
                    if k == kind {
                        return Some(&attrs[4..len]);
                    }
                    let aligned = (len + 3) & !3;
                    if aligned > attrs.len() {
                        return None;
                    }
                    attrs = &attrs[aligned..];
                }
                None
            }
        }
    }
}

pub mod introspect {
    //! Localhost HTTP/JSON introspection API (see FILIN_HTTP_INTROSPECT.md).
    //!
    //! Exposes filin's live state (current channel, master, peer table, sync
    //! health, mDNS-reception counters, per-channel rebroadcast counts) plus a
    //! runtime-toggleable ring-buffer trace, over a loopback-only HTTP server.
    //! The hot path updates atomic counters and refreshes a `RwLock` snapshot;
    //! the HTTP thread reads — it never blocks the AF_PACKET inject/receive
    //! loop.
    //!
    //! Lock-free where it matters: hot-loop bumps are `AtomicU64::fetch_add`.
    //! Snapshot reads on the HTTP path take a brief `RwLock::read`; snapshot
    //! writes on the hot path are throttled to ~10 Hz.
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, RwLock};

    use serde::{Deserialize, Serialize};

    /// Format an AWDL MAC address as `xx:xx:xx:xx:xx:xx` for JSON consumers.
    pub fn mac_string(addr: [u8; 6]) -> String {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            addr[0], addr[1], addr[2], addr[3], addr[4], addr[5]
        )
    }

    /// Capacity of the `last_master_macs` ring in [`Counters`] — small enough
    /// to be cheap, large enough to see cluster churn at a glance
    /// (FILIN_SYNC_QUALITY.md Part A).
    pub const LAST_MASTER_MACS_CAP: usize = 8;

    /// Hot-path counters for the introspection API. `mdns_rx_*` are plain
    /// `AtomicU64` (bumped per-packet on the AF_PACKET loop). `rebroadcast`
    /// and `last_master_macs` are `Mutex`-guarded — only touched a few times
    /// per second (rate-limited re-broadcasts / master changes), so the locks
    /// are uncontended. `aw_alignment` is a pair of atomics.
    #[derive(Debug)]
    pub struct Counters {
        mdns_rx_self: AtomicU64,
        mdns_rx_other: AtomicU64,
        rebroadcast: std::sync::Mutex<std::collections::BTreeMap<u8, u64>>,
        /// FILIN_SYNC_QUALITY.md Part A: how many times the committed master
        /// MAC changed. Bumped once per real switch.
        master_changes: AtomicU64,
        /// How many times the cluster TOP master (election.master_addr)
        /// changed — the signal of true cluster churn, as opposed to
        /// re-parenting (sync_addr) among peers sharing one top master.
        master_addr_changes: AtomicU64,
        /// How many times our direct PARENT (election.sync_addr) changed.
        /// High relative to master_addr_changes = re-parenting churn.
        sync_addr_changes: AtomicU64,
        /// Availability-window alignment samples: aligned vs total. Bumped on
        /// each sync update from the elected master; `pct = aligned/total`.
        aw_aligned: AtomicU64,
        aw_total: AtomicU64,
        /// Ring of recent master MACs (cap [`LAST_MASTER_MACS_CAP`]) to
        /// visualize cluster churn in `/status`.
        last_master_macs: std::sync::Mutex<std::collections::VecDeque<String>>,
    }

    impl Default for Counters {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Slowly-changing state exposed by `GET /status`. The fast counter-backed
    /// fields (`mdns_rx_*`, `rebroadcast_counts`) live on [`Counters`] and are
    /// merged into the JSON response at request time so the hot path never
    /// takes a lock to update them.
    #[derive(Debug, Clone, PartialEq, Serialize, Default)]
    pub struct StatusSnapshot {
        pub current_channel: u8,
        pub master_mac: String,
        /// The sequence control of the most-recently-injected AWDL frame.
        pub master_seq: u16,
        pub synced: bool,
        pub peer_count: usize,
        /// The adopted master's anchor channel (slot-0 / dominant). 0 when
        /// self-master or no master. Surfaced in /status for debugging.
        pub master_anchor: u8,
        /// Last sampled offset between our TSF bridge and the adopted
        /// master's (microseconds, magnitude). 0 if no master yet. The thing
        /// that must stay small for tight AWDL sync (FILIN_SYNC_QUALITY.md).
        pub tsf_offset_us: i64,
        /// Milliseconds since the current master was adopted. Grows while we
        /// hold one master; resets on each switch. Large + growing = stable.
        pub master_age_ms: u64,
        /// Our direct PARENT's MAC (election.sync_addr) — the peer we
        /// currently sync to. Distinct from `master_mac` (the cluster's top
        /// master_addr). Exposed so live debugging can tell re-parenting
        /// churn (sync_addr flips) from real cluster churn (master_addr
        /// flips). FILIN_SYNC_QUALITY.md refinement.
        pub sync_addr_mac: String,
    }

    /// Capacity of the trace ring buffer (FILIN_HTTP_INTROSPECT.md: ~2000).
    pub const TRACE_RING_CAP: usize = 2000;

    /// One captured `tracing` event, serialized for `GET /trace`.
    #[derive(Debug, Clone, Serialize, PartialEq)]
    pub struct TraceEvent {
        /// Wall-clock milliseconds since UNIX_EPOCH (jq-friendly).
        pub ts: u64,
        /// `ERROR`/`WARN`/`INFO`/`DEBUG`/`TRACE`.
        pub level: String,
        /// `tracing` target (usually the module path).
        pub target: String,
        /// The event's format-string message.
        pub msg: String,
        /// Structured fields collected via `tracing::field::Visit`.
        pub fields: serde_json::Map<String, serde_json::Value>,
    }

    /// Fixed-capacity ring buffer of [`TraceEvent`]s. `push` is O(1); the
    /// mutex is held only briefly. Drained only by the HTTP thread.
    pub struct TraceRing {
        cap: usize,
        buf: std::sync::Mutex<std::collections::VecDeque<TraceEvent>>,
    }

    impl TraceRing {
        pub fn new(cap: usize) -> Self {
            Self {
                cap,
                buf: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(cap)),
            }
        }

        pub fn push(&self, ev: TraceEvent) {
            if let Ok(mut buf) = self.buf.lock() {
                if buf.len() >= self.cap {
                    buf.pop_front();
                }
                buf.push_back(ev);
            }
        }

        /// Last `n` events in insertion order (oldest-first). `n == 0` → empty.
        pub fn last_n(&self, n: usize) -> Vec<TraceEvent> {
            if n == 0 {
                return Vec::new();
            }
            let buf = match self.buf.lock() {
                Ok(b) => b,
                Err(e) => e.into_inner(),
            };
            let len = buf.len();
            let start = len.saturating_sub(n);
            buf.iter().skip(start).cloned().collect()
        }

        pub fn len(&self) -> usize {
            self.buf
                .lock()
                .map(|b| b.len())
                .unwrap_or_else(|e| e.into_inner().len())
        }

        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }
    }

    /// User-facing capture level set via `POST /trace {"level": ...}`. The
    /// encoding matches `tracing`'s `Level::as_usize()` so the gate check is a
    /// single `>=` against an event's level number.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GateLevel {
        Off,
        Debug,
        Trace,
    }

    impl GateLevel {
        /// `Off=0`, `Debug=4` (== `tracing::Level::DEBUG`), `Trace=5`.
        pub fn as_u8(self) -> u8 {
            match self {
                GateLevel::Off => 0,
                GateLevel::Debug => 4,
                GateLevel::Trace => 5,
            }
        }

        /// Parse the strings accepted by `POST /trace {"level": ...}`.
        /// Case-sensitive per the spec (`off|debug|trace`).
        pub fn from_request_str(s: &str) -> Option<Self> {
            match s {
                "off" => Some(Self::Off),
                "debug" => Some(Self::Debug),
                "trace" => Some(Self::Trace),
                _ => None,
            }
        }
    }

    /// Atomic gate read on every tracing event (hot path). The cost when
    /// disabled is a single `Relaxed` atomic load + compare.
    pub struct TraceGate(std::sync::atomic::AtomicU8);

    impl TraceGate {
        pub const fn new() -> Self {
            Self(std::sync::atomic::AtomicU8::new(0))
        }

        pub fn level_u8(&self) -> u8 {
            self.0.load(std::sync::atomic::Ordering::Relaxed)
        }

        pub fn set(&self, level: GateLevel) {
            self.0.store(level.as_u8(), std::sync::atomic::Ordering::Relaxed);
        }

        /// `event_level_num` is `tracing::Level::as_usize()` (ERROR=1 … TRACE=5).
        /// Returns true iff capture is enabled and the event is verbose enough.
        pub fn allows(&self, event_level_num: u8) -> bool {
            let g = self.0.load(std::sync::atomic::Ordering::Relaxed);
            g != 0 && event_level_num != 0 && g >= event_level_num
        }
    }

    impl Default for TraceGate {
        fn default() -> Self {
            Self::new()
        }
    }

    /// `tracing::field::Visit` impl that pulls an event's message + structured
    /// fields out of the macro machinery. The special `"message"` field (set
    /// by `tracing::info!("fmt", ...)` macros) is routed to `TraceEvent::msg`;
    /// everything else lands in `fields` with type-aware JSON values.
    #[derive(Default)]
    struct FieldCollector {
        message: Option<String>,
        fields: serde_json::Map<String, serde_json::Value>,
    }

    impl FieldCollector {
        fn route(&mut self, name: &str, value: serde_json::Value) {
            if name == "message" {
                if let serde_json::Value::String(s) = value {
                    self.message = Some(s);
                } else {
                    self.message = Some(value.to_string());
                }
            } else {
                self.fields.insert(name.to_string(), value);
            }
        }
    }

    impl tracing::field::Visit for FieldCollector {
        fn record_debug(
            &mut self,
            field: &tracing::field::Field,
            value: &dyn std::fmt::Debug,
        ) {
            self.route(field.name(), serde_json::Value::String(format!("{value:?}")));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.route(field.name(), serde_json::Value::String(value.to_string()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.route(field.name(), serde_json::json!(value));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.route(field.name(), serde_json::json!(value));
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.route(field.name(), serde_json::json!(value));
        }

        fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
            self.route(field.name(), serde_json::json!(value));
        }
    }

    /// Wall-clock milliseconds since `UNIX_EPOCH` for `TraceEvent::ts`. Falls
    /// back to 0 if the clock is before the epoch (won't happen in practice).
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Map a `tracing::Level` to its verbosity rank (ERROR=1 … TRACE=5), the
    /// same encoding used by [`GateLevel`] and [`TraceGate`]. Kept explicit so
    /// we don't depend on `LevelFilter::as_usize` (sealed in `tracing-core`).
    fn level_to_u8(level: &tracing::Level) -> u8 {
        match *level {
            tracing::Level::ERROR => 1,
            tracing::Level::WARN => 2,
            tracing::Level::INFO => 3,
            tracing::Level::DEBUG => 4,
            tracing::Level::TRACE => 5,
        }
    }

    /// A `tracing_subscriber::Layer` that, when the [`TraceGate`] permits,
    /// serializes each event into the [`TraceRing`]. The cost when the gate is
    /// off is a single relaxed atomic load + compare; the hot loop is
    /// unaffected. `FieldCollector` is reused on the (rare) capture path.
    pub struct TraceLayer {
        ring: Arc<TraceRing>,
        gate: Arc<TraceGate>,
    }

    impl TraceLayer {
        pub fn new(ring: Arc<TraceRing>, gate: Arc<TraceGate>) -> Self {
            Self { ring, gate }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for TraceLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            let level_num = level_to_u8(metadata.level());
            if !self.gate.allows(level_num) {
                return;
            }
            let mut collector = FieldCollector::default();
            event.record(&mut collector);
            let ev = TraceEvent {
                ts: now_ms(),
                level: metadata.level().as_str().to_string(),
                target: metadata.target().to_string(),
                msg: collector.message.unwrap_or_default(),
                fields: collector.fields,
            };
            self.ring.push(ev);
        }
    }

    /// One row of `GET /peers`. The runtime builds these from `awdl_state.peers`
    /// (`mac`, `decoded_chanseq`) plus `current_channel` and the host clock.
    #[derive(Debug, Clone, Serialize, PartialEq)]
    pub struct PeerView {
        pub mac: String,
        pub last_seen_ms_ago: u64,
        pub decoded_chanseq: Vec<u8>,
        pub current_channel: u8,
    }

    /// Minimal HTTP request view consumed by the pure [`route`] dispatcher.
    /// `path` excludes the query string; `query` is the text after `?`.
    #[derive(Debug, Clone, Copy)]
    pub struct Request<'a> {
        pub method: &'a str,
        pub path: &'a str,
        pub query: &'a str,
        pub body: &'a [u8],
    }

    /// Minimal HTTP response produced by [`route`] and serialized by the
    /// loopback server thread.
    #[derive(Debug, Clone)]
    pub struct Response {
        pub status: u16,
        pub content_type: &'static str,
        pub body: Vec<u8>,
    }

    impl Response {
        fn json<T: Serialize>(status: u16, value: &T) -> Self {
            Self {
                status,
                content_type: "application/json",
                body: serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec()),
            }
        }

        fn text(status: u16, body: &str) -> Self {
            Self {
                status,
                content_type: "text/plain; charset=utf-8",
                body: body.as_bytes().to_vec(),
            }
        }
    }

    /// Default `n` for `GET /trace` when the query is missing/invalid.
    pub const TRACE_DEFAULT_N: usize = 300;

    /// Body schema for `POST /trace` (`{"level":"off|debug|trace"}`).
    #[derive(Debug, Deserialize)]
    struct TracePostBody {
        level: String,
    }

    /// Pure HTTP request dispatcher. Reads from the [`Introspection`] snapshot
    /// and counters — no socket I/O, fully unit-testable. The loopback server
    /// thread just parses the request line and calls this.
    pub fn route(intro: &Introspection, req: &Request<'_>) -> Response {
        match (req.method, req.path) {
            ("GET", "/status") => get_status(intro),
            ("GET", "/peers") => get_peers(intro),
            ("GET", "/trace") => get_trace(intro, req.query),
            ("POST", "/trace") => post_trace(intro, req.body),
            _ => Response::text(404, "not found"),
        }
    }

    fn get_status(intro: &Introspection) -> Response {
        let snapshot = intro.status();
        let mut rebroadcast = serde_json::Map::new();
        for (ch, count) in intro.counters.rebroadcast_pairs() {
            rebroadcast.insert(ch.to_string(), serde_json::json!(count));
        }
        #[derive(Serialize)]
        struct StatusResponse {
            current_channel: u8,
            master_mac: String,
            master_seq: u16,
            synced: bool,
            peer_count: usize,
            master_anchor: u8,
            rebroadcast_counts: serde_json::Map<String, serde_json::Value>,
            mdns_rx_self: u64,
            mdns_rx_other: u64,
            // FILIN_SYNC_QUALITY.md Part A: sync-quality metrics.
            tsf_offset_us: u64,
            master_age_ms: u64,
            master_changes: u64,
            aw_alignment_pct: u8,
            last_master_macs: Vec<String>,
            sync_addr_mac: String,
            master_addr_changes: u64,
            sync_addr_changes: u64,
        }
        Response::json(
            200,
            &StatusResponse {
                current_channel: snapshot.current_channel,
                master_mac: snapshot.master_mac,
                master_seq: snapshot.master_seq,
                synced: snapshot.synced,
                peer_count: snapshot.peer_count,
                master_anchor: snapshot.master_anchor,
                rebroadcast_counts: rebroadcast,
                mdns_rx_self: intro.counters.mdns_rx_self(),
                mdns_rx_other: intro.counters.mdns_rx_other(),
                tsf_offset_us: snapshot.tsf_offset_us.unsigned_abs(),
                master_age_ms: snapshot.master_age_ms,
                master_changes: intro.counters.master_changes(),
                aw_alignment_pct: intro.counters.aw_alignment_pct(),
                last_master_macs: intro.counters.last_master_macs(),
                sync_addr_mac: snapshot.sync_addr_mac,
                master_addr_changes: intro.counters.master_addr_changes(),
                sync_addr_changes: intro.counters.sync_addr_changes(),
            },
        )
    }

    fn get_peers(intro: &Introspection) -> Response {
        let peers = intro.peers();
        Response::json(200, &peers)
    }

    fn get_trace(intro: &Introspection, query: &str) -> Response {
        let n = parse_trace_n(query).unwrap_or(TRACE_DEFAULT_N);
        let events = intro.trace_ring().last_n(n);
        Response::json(200, &events)
    }

    fn post_trace(intro: &Introspection, body: &[u8]) -> Response {
        let parsed: TracePostBody = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(_) => return Response::text(400, "invalid JSON body"),
        };
        let level = match GateLevel::from_request_str(&parsed.level) {
            Some(l) => l,
            None => {
                return Response::text(400, "unknown level; expected off|debug|trace")
            }
        };
        intro.trace_gate().set(level);
        Response::text(200, "ok")
    }

    /// Extract `n` from a query string like `n=300` (ignoring other pairs).
    fn parse_trace_n(query: &str) -> Option<usize> {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "n" {
                    return v.parse::<usize>().ok();
                }
            }
        }
        None
    }

    /// Parsed HTTP request line. `path` excludes the query string.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ParsedRequestLine<'a> {
        pub method: &'a str,
        pub path: &'a str,
        pub query: &'a str,
    }

    /// Parse an HTTP request line like `GET /trace?n=300 HTTP/1.1\r\n`.
    /// Returns `None` if the line doesn't have at least `METHOD TARGET` tokens.
    pub fn parse_request_line(line: &str) -> Option<ParsedRequestLine<'_>> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let mut parts = trimmed.split_whitespace();
        let method = parts.next()?;
        let target = parts.next()?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        Some(ParsedRequestLine { method, path, query })
    }

    /// Reason phrase for the few status codes this minimal server emits.
    fn reason_phrase(status: u16) -> &'static str {
        match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "OK",
        }
    }

    /// Serialize a [`Response`] as a complete HTTP/1.1 message (status line +
    /// headers + blank line + body). `Connection: close` so the client reads
    /// the body until EOF — no chunked encoding or keep-alive to worry about.
    pub fn format_response(resp: &Response) -> Vec<u8> {
        let reason = reason_phrase(resp.status);
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
            status = resp.status,
            ct = resp.content_type,
            len = resp.body.len(),
        );
        let mut out = Vec::with_capacity(head.len() + resp.body.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(&resp.body);
        out
    }

    /// Serve introspection requests on a dedicated thread. The `listener` is
    /// supplied (not bound here) so the caller can pick an ephemeral port in
    /// tests via `TcpListener::bind("127.0.0.1:0")` + `local_addr()`. Bind to
    /// `127.0.0.1` only — never expose this off-box (the peer-address check
    /// below is defense-in-depth).
    pub fn spawn(
        intro: Arc<Introspection>,
        listener: std::net::TcpListener,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("filin-introspect".into())
            .spawn(move || serve_loop(intro, listener))
            .expect("spawn filin-introspect thread")
    }

    fn serve_loop(intro: Arc<Introspection>, listener: std::net::TcpListener) {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(err) => {
                    tracing::trace!(?err, "introspection accept failed");
                    continue;
                }
            };
            // Defense-in-depth: never reply to non-loopback peers.
            let is_loopback = stream
                .peer_addr()
                .map(|a| a.ip().is_loopback())
                .unwrap_or(false);
            if !is_loopback {
                continue;
            }
            if let Err(err) = handle_connection(&intro, stream) {
                tracing::trace!(?err, "introspection connection ended");
            }
        }
    }

    fn handle_connection(
        intro: &Introspection,
        mut stream: std::net::TcpStream,
    ) -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Read, Write};

        let mut reader = BufReader::new(&mut stream);

        // Request line: "METHOD TARGET HTTP/1.x"
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(()); // empty connection
        }
        let Some(parsed) = parse_request_line(&request_line) else {
            stream.write_all(&format_response(&Response::text(400, "bad request")))?;
            return Ok(());
        };

        // Headers: read until blank line, pick out Content-Length.
        let mut content_length: usize = 0;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                break;
            }
            let trimmed = header.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }

        // Body (only POST /trace uses this, and it's tiny).
        let mut body = Vec::new();
        if content_length > 0 {
            body.resize(content_length, 0u8);
            reader.read_exact(&mut body)?;
        }

        let response = route(
            intro,
            &Request {
                method: parsed.method,
                path: parsed.path,
                query: parsed.query,
                body: &body,
            },
        );
        stream.write_all(&format_response(&response))?;
        Ok(())
    }

    /// Aggregate introspection state, shared (via `Arc`) between the AF_PACKET
    /// hot loop, the optional `TraceLayer`, and the HTTP server thread.
    pub struct Introspection {
        pub counters: Counters,
        status: RwLock<StatusSnapshot>,
        peers: RwLock<Vec<PeerView>>,
        trace_ring: Arc<TraceRing>,
        trace_gate: Arc<TraceGate>,
    }

    impl Introspection {
        /// Construct a fresh introspection handle wrapped in an `Arc` — the
        /// shape the runtime, TraceLayer, and HTTP thread all share. Owns a
        /// private ring + gate so the layer and the HTTP server share state.
        pub fn new() -> Arc<Self> {
            Self::new_with_trace(
                Arc::new(TraceRing::new(TRACE_RING_CAP)),
                Arc::new(TraceGate::new()),
            )
        }

        /// Construct with caller-supplied ring/gate handles — useful when the
        /// `TraceLayer` needs to be wired to the exact same gate the HTTP
        /// `POST /trace` handler flips.
        pub fn new_with_trace(ring: Arc<TraceRing>, gate: Arc<TraceGate>) -> Arc<Self> {
            Arc::new(Self {
                counters: Counters::new(),
                status: RwLock::new(StatusSnapshot::default()),
                peers: RwLock::new(Vec::new()),
                trace_ring: ring,
                trace_gate: gate,
            })
        }

        /// Replace the status snapshot. Called from the hot loop on a throttled
        /// cadence; reads happen on the HTTP thread.
        pub fn set_status(&self, snapshot: StatusSnapshot) {
            if let Ok(mut guard) = self.status.write() {
                *guard = snapshot;
            }
        }

        /// Snapshot clone for serialization. Returns the default snapshot if the
        /// lock is poisoned (the daemon keeps running).
        pub fn status(&self) -> StatusSnapshot {
            match self.status.read() {
                Ok(guard) => guard.clone(),
                Err(e) => e.into_inner().clone(),
            }
        }

        /// Replace the per-peer view (built by the runtime from `awdl_state.peers`).
        pub fn set_peers(&self, peers: Vec<PeerView>) {
            if let Ok(mut guard) = self.peers.write() {
                *guard = peers;
            }
        }

        /// Per-peer view clone for `GET /peers`.
        pub fn peers(&self) -> Vec<PeerView> {
            match self.peers.read() {
                Ok(guard) => guard.clone(),
                Err(e) => e.into_inner().clone(),
            }
        }

        /// The ring backing `GET /trace` and the `TraceLayer`.
        pub fn trace_ring(&self) -> &Arc<TraceRing> {
            &self.trace_ring
        }

        /// The gate flipped by `POST /trace` and read by the `TraceLayer`.
        pub fn trace_gate(&self) -> &Arc<TraceGate> {
            &self.trace_gate
        }

        /// Convenience: build a `TraceLayer` already wired to this handle's
        /// ring + gate.
        pub fn trace_layer(&self) -> TraceLayer {
            TraceLayer::new(self.trace_ring.clone(), self.trace_gate.clone())
        }
    }

    impl Counters {
        pub fn new() -> Self {
            Self {
                mdns_rx_self: AtomicU64::new(0),
                mdns_rx_other: AtomicU64::new(0),
                rebroadcast: std::sync::Mutex::new(std::collections::BTreeMap::new()),
                master_changes: AtomicU64::new(0),
                master_addr_changes: AtomicU64::new(0),
                sync_addr_changes: AtomicU64::new(0),
                aw_aligned: AtomicU64::new(0),
                aw_total: AtomicU64::new(0),
                last_master_macs: std::sync::Mutex::new(std::collections::VecDeque::new()),
            }
        }

        pub fn inc_mdns_rx_self(&self) {
            self.mdns_rx_self.fetch_add(1, Ordering::Relaxed);
        }

        pub fn inc_mdns_rx_other(&self) {
            self.mdns_rx_other.fetch_add(1, Ordering::Relaxed);
        }

        pub fn inc_rebroadcast(&self, channel: u8) {
            if let Ok(mut map) = self.rebroadcast.lock() {
                *map.entry(channel).or_insert(0) += 1;
            }
        }

        pub fn mdns_rx_self(&self) -> u64 {
            self.mdns_rx_self.load(Ordering::Relaxed)
        }

        pub fn mdns_rx_other(&self) -> u64 {
            self.mdns_rx_other.load(Ordering::Relaxed)
        }

        pub fn rebroadcast_for(&self, channel: u8) -> u64 {
            self.rebroadcast
                .lock()
                .ok()
                .and_then(|map| map.get(&channel).copied())
                .unwrap_or(0)
        }

        pub fn rebroadcast_pairs(&self) -> Vec<(u8, u64)> {
            self.rebroadcast
                .lock()
                .map(|map| map.iter().map(|(&ch, &count)| (ch, count)).collect())
                .unwrap_or_default()
        }

        /// Bump the master-change counter (called once when the committed
        /// master MAC actually changes).
        pub fn inc_master_change(&self) {
            self.master_changes.fetch_add(1, Ordering::Relaxed);
        }

        pub fn master_changes(&self) -> u64 {
            self.master_changes.load(Ordering::Relaxed)
        }

        /// Bump the cluster-top-master (master_addr) change counter.
        pub fn inc_master_addr_change(&self) {
            self.master_addr_changes.fetch_add(1, Ordering::Relaxed);
        }

        pub fn master_addr_changes(&self) -> u64 {
            self.master_addr_changes.load(Ordering::Relaxed)
        }

        /// Bump the parent (sync_addr) change counter.
        pub fn inc_sync_addr_change(&self) {
            self.sync_addr_changes.fetch_add(1, Ordering::Relaxed);
        }

        pub fn sync_addr_changes(&self) -> u64 {
            self.sync_addr_changes.load(Ordering::Relaxed)
        }

        /// Append a master MAC string to the recent-masters ring; the oldest
        /// is dropped once the cap is exceeded.
        pub fn push_master_mac(&self, mac: String) {
            if let Ok(mut ring) = self.last_master_macs.lock() {
                if ring.len() >= LAST_MASTER_MACS_CAP {
                    ring.pop_front();
                }
                ring.push_back(mac);
            }
        }

        pub fn last_master_macs(&self) -> Vec<String> {
            self.last_master_macs
                .lock()
                .map(|ring| ring.iter().cloned().collect())
                .unwrap_or_default()
        }

        /// Record one availability-window alignment sample. `aligned` true iff
        /// the sync error was within the acceptable bound this round.
        pub fn record_aw_check(&self, aligned: bool) {
            self.aw_total.fetch_add(1, Ordering::Relaxed);
            if aligned {
                self.aw_aligned.fetch_add(1, Ordering::Relaxed);
            }
        }

        /// Percentage of AW samples that were aligned (0..=100). 0 when no
        /// samples have been recorded yet.
        pub fn aw_alignment_pct(&self) -> u8 {
            let total = self.aw_total.load(Ordering::Relaxed);
            if total == 0 {
                return 0;
            }
            let aligned = self.aw_aligned.load(Ordering::Relaxed);
            let pct = aligned.saturating_mul(100) / total;
            pct.min(100) as u8
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn counters_start_at_zero_for_social_channels() {
            let c = Counters::new();
            assert_eq!(c.mdns_rx_self(), 0);
            assert_eq!(c.mdns_rx_other(), 0);
            assert_eq!(c.rebroadcast_for(6), 0);
            assert_eq!(c.rebroadcast_for(44), 0);
            assert_eq!(c.rebroadcast_for(149), 0);
        }

        #[test]
        fn mdns_and_rebroadcast_counters_are_independent_atomics() {
            let c = Counters::new();
            c.inc_mdns_rx_self();
            c.inc_mdns_rx_self();
            c.inc_mdns_rx_other();
            c.inc_rebroadcast(44);
            c.inc_rebroadcast(44);
            c.inc_rebroadcast(44);
            c.inc_rebroadcast(149);

            assert_eq!(c.mdns_rx_self(), 2);
            assert_eq!(c.mdns_rx_other(), 1);
            assert_eq!(c.rebroadcast_for(44), 3);
            assert_eq!(c.rebroadcast_for(149), 1);
            assert_eq!(c.rebroadcast_for(6), 0);
        }

        #[test]
        fn rebroadcast_pairs_round_trips_all_social_channels() {
            let c = Counters::new();
            c.inc_rebroadcast(6);
            c.inc_rebroadcast(149);
            c.inc_rebroadcast(149);

            // Adaptive map: only channels that were bumped appear.
            assert_eq!(c.rebroadcast_pairs(), vec![(6, 1), (149, 2)]);
        }

        #[test]
        fn rebroadcast_counter_tracks_arbitrary_anchor_channel() {
            // FILIN_ANCHOR_REBROADCAST.md: /status rebroadcast_counts must
            // show the anchor channel (e.g. 52) incrementing — not just the
            // hardcoded 6/44/149.
            let c = Counters::new();
            c.inc_rebroadcast(52);
            c.inc_rebroadcast(52);
            c.inc_rebroadcast(52);
            assert_eq!(c.rebroadcast_for(52), 3);
            assert!(c.rebroadcast_pairs().contains(&(52, 3)));
        }

        // --- sync-quality metrics (FILIN_SYNC_QUALITY.md Part A) ---

        #[test]
        fn master_change_counter_increments_per_change() {
            let c = Counters::new();
            assert_eq!(c.master_changes(), 0);
            c.inc_master_change();
            c.inc_master_change();
            assert_eq!(c.master_changes(), 2);
        }

        #[test]
        fn last_master_macs_ring_caps_at_capacity_oldest_dropped() {
            let c = Counters::new();
            for i in 0..(LAST_MASTER_MACS_CAP + 3) {
                c.push_master_mac(format!("02:00:00:00:00:{:02x}", i));
            }
            let macs = c.last_master_macs();
            assert_eq!(macs.len(), LAST_MASTER_MACS_CAP);
            // Oldest dropped; the newest is the last pushed.
            assert_eq!(
                macs.last().map(String::as_str),
                Some(&*format!("02:00:00:00:00:{:02x}", LAST_MASTER_MACS_CAP + 2))
            );
        }

        #[test]
        fn last_master_macs_ring_preserves_insertion_order() {
            let c = Counters::new();
            c.push_master_mac("02:aa:bb:cc:dd:ee".into());
            c.push_master_mac("02:00:00:00:00:01".into());
            c.push_master_mac("02:00:00:00:00:02".into());
            assert_eq!(
                c.last_master_macs(),
                vec![
                    "02:aa:bb:cc:dd:ee".to_string(),
                    "02:00:00:00:00:01".to_string(),
                    "02:00:00:00:00:02".to_string(),
                ]
            );
        }

        #[test]
        fn aw_alignment_pct_starts_at_zero_with_no_samples() {
            let c = Counters::new();
            assert_eq!(c.aw_alignment_pct(), 0);
        }

        #[test]
        fn aw_alignment_pct_computes_aligned_fraction() {
            let c = Counters::new();
            c.record_aw_check(true);
            c.record_aw_check(true);
            c.record_aw_check(false);
            c.record_aw_check(true);
            // 3 aligned out of 4 total = 75%.
            assert_eq!(c.aw_alignment_pct(), 75);
        }

        #[test]
        fn aw_alignment_pct_clamps_to_100() {
            let c = Counters::new();
            for _ in 0..10 {
                c.record_aw_check(true);
            }
            assert_eq!(c.aw_alignment_pct(), 100);
        }

        // --- master_addr (cluster top) vs sync_addr (parent) churn metrics
        //     (FILIN_SYNC_QUALITY.md refinement: the apparent "master churn"
        //     is re-PARENTING among peers sharing the same top master_addr). ---

        #[test]
        fn master_addr_and_sync_addr_change_counters_are_independent() {
            let c = Counters::new();
            assert_eq!(c.master_addr_changes(), 0);
            assert_eq!(c.sync_addr_changes(), 0);
            c.inc_sync_addr_change();
            c.inc_sync_addr_change();
            c.inc_master_addr_change();
            assert_eq!(c.sync_addr_changes(), 2);
            assert_eq!(c.master_addr_changes(), 1);
        }

        #[test]
        fn status_snapshot_serializes_to_required_json_keys() {
            let snapshot = StatusSnapshot {
                current_channel: 44,
                master_mac: mac_string([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
                master_seq: 7,
                synced: true,
                peer_count: 2,
                master_anchor: 52,
                tsf_offset_us: 1200,
                master_age_ms: 4500,
                sync_addr_mac: mac_string([0x02, 0xcc, 0, 0, 0, 1]),
            };

            let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
            let obj = json.as_object().expect("snapshot is a JSON object");

            assert_eq!(obj.get("current_channel").and_then(|v| v.as_u64()), Some(44));
            assert_eq!(obj.get("master_anchor").and_then(|v| v.as_u64()), Some(52));
            assert_eq!(
                obj.get("tsf_offset_us").and_then(|v| v.as_u64()),
                Some(1200),
                "tsf_offset_us must serialize"
            );
            assert_eq!(
                obj.get("master_age_ms").and_then(|v| v.as_u64()),
                Some(4500),
                "master_age_ms must serialize"
            );
            assert_eq!(
                obj.get("sync_addr_mac").and_then(|v| v.as_str()),
                Some("02:cc:00:00:00:01"),
                "sync_addr_mac (our parent) must serialize — distinct from master_mac (top master)"
            );
            assert_eq!(
                obj.get("master_mac").and_then(|v| v.as_str()),
                Some("02:aa:bb:cc:dd:ee")
            );
            assert_eq!(obj.get("master_seq").and_then(|v| v.as_u64()), Some(7));
            assert_eq!(obj.get("synced").and_then(|v| v.as_bool()), Some(true));
            assert_eq!(obj.get("peer_count").and_then(|v| v.as_u64()), Some(2));
            // The counter-backed fields live on Counters, not the snapshot —
            // they are merged into /status at response time, so they must NOT
            // be part of the snapshot's own JSON shape.
            assert!(
                !obj.contains_key("rebroadcast_counts"),
                "rebroadcast_counts belong to Counters, not the snapshot"
            );
        }

        #[test]
        fn introspection_round_trips_status_through_rwlock() {
            let intro = Introspection::new();
            let snapshot = StatusSnapshot {
                current_channel: 149,
                master_mac: "02:00:00:00:00:01".into(),
                master_seq: 42,
                synced: false,
                peer_count: 1,
                master_anchor: 0,
                tsf_offset_us: 0,
                master_age_ms: 0,
                sync_addr_mac: "02:00:00:00:00:01".into(),
            };
            intro.set_status(snapshot.clone());

            assert_eq!(intro.status(), snapshot);
        }

        #[test]
        fn introspection_shares_counters_and_status_behind_one_arc() {
            // The runtime holds Introspection as a single Arc and bumps
            // counters on the hot path while refreshing status less often.
            let intro = Introspection::new();
            intro.counters.inc_mdns_rx_other();
            intro.counters.inc_rebroadcast(6);
            intro.set_status(StatusSnapshot {
                current_channel: 6,
                master_mac: mac_string([0x02, 0, 0, 0, 0, 1]),
                master_seq: 3,
                synced: true,
                peer_count: 4,
                master_anchor: 0,
                tsf_offset_us: 0,
                master_age_ms: 0,
                sync_addr_mac: mac_string([0x02, 0, 0, 0, 0, 1]),
            });

            assert_eq!(intro.counters.mdns_rx_other(), 1);
            assert_eq!(intro.counters.rebroadcast_for(6), 1);
            assert_eq!(intro.status().peer_count, 4);
        }

        fn ev(ts: u64) -> TraceEvent {
            TraceEvent {
                ts,
                level: "INFO".into(),
                target: "filin".into(),
                msg: format!("event {ts}"),
                fields: serde_json::Map::new(),
            }
        }

        #[test]
        fn trace_ring_caps_at_capacity_dropping_oldest() {
            let ring = TraceRing::new(3);
            ring.push(ev(1));
            ring.push(ev(2));
            ring.push(ev(3));
            ring.push(ev(4)); // evicts ev(1)

            assert_eq!(ring.len(), 3);
            let ts: Vec<u64> = ring.last_n(10).into_iter().map(|e| e.ts).collect();
            assert_eq!(ts, vec![2, 3, 4]);
        }

        #[test]
        fn trace_ring_last_n_returns_tail_in_insertion_order() {
            let ring = TraceRing::new(super::TRACE_RING_CAP);
            for i in 0..10 {
                ring.push(ev(i));
            }

            let ts: Vec<u64> = ring.last_n(3).into_iter().map(|e| e.ts).collect();
            assert_eq!(ts, vec![7, 8, 9]);
        }

        #[test]
        fn trace_ring_last_n_handles_zero_and_overflow() {
            let ring = TraceRing::new(super::TRACE_RING_CAP);
            ring.push(ev(5));

            assert!(ring.last_n(0).is_empty());
            assert_eq!(ring.last_n(99).len(), 1);
        }

        #[test]
        fn trace_ring_serializes_events_with_required_shape() {
            let ring = TraceRing::new(super::TRACE_RING_CAP);
            let mut fields = serde_json::Map::new();
            fields.insert("sent".into(), serde_json::json!(1));
            ring.push(TraceEvent {
                ts: 1_700_000_000_000,
                level: "DEBUG".into(),
                target: "filin_rs::runtime".into(),
                msg: "injected awdl announce frame".into(),
                fields,
            });

            let json = serde_json::to_value(&ring.last_n(1)[0]).expect("event serializes");
            let obj = json.as_object().expect("event is object");
            assert_eq!(
                obj.get("ts").and_then(|v| v.as_u64()),
                Some(1_700_000_000_000)
            );
            assert_eq!(obj.get("level").and_then(|v| v.as_str()), Some("DEBUG"));
            assert_eq!(
                obj.get("target").and_then(|v| v.as_str()),
                Some("filin_rs::runtime")
            );
            assert_eq!(
                obj.get("msg").and_then(|v| v.as_str()),
                Some("injected awdl announce frame")
            );
            assert_eq!(
                obj.get("fields").and_then(|v| v.get("sent")).and_then(|v| v.as_u64()),
                Some(1)
            );
        }

        #[test]
        fn trace_gate_defaults_to_off_and_gates_by_tracing_level_num() {
            // tracing Level num: ERROR=1, WARN=2, INFO=3, DEBUG=4, TRACE=5.
            let gate = TraceGate::new();
            assert_eq!(gate.level_u8(), 0);
            assert!(!gate.allows(1)); // off gates even ERROR

            gate.set(GateLevel::Debug);
            assert!(gate.allows(1)); // ERROR
            assert!(gate.allows(3)); // INFO
            assert!(gate.allows(4)); // DEBUG
            assert!(!gate.allows(5)); // DEBUG does not capture TRACE

            gate.set(GateLevel::Trace);
            assert!(gate.allows(5)); // TRACE captures everything

            gate.set(GateLevel::Off);
            assert!(!gate.allows(1));
        }

        #[test]
        fn gate_level_parses_post_body_strings() {
            assert_eq!(GateLevel::from_request_str("off"), Some(GateLevel::Off));
            assert_eq!(GateLevel::from_request_str("debug"), Some(GateLevel::Debug));
            assert_eq!(GateLevel::from_request_str("trace"), Some(GateLevel::Trace));
            assert_eq!(GateLevel::from_request_str("bogus"), None);
            assert_eq!(GateLevel::from_request_str("INFO"), None); // case-sensitive per spec
        }

        fn run_with_layer<F: FnOnce()>(gate: GateLevel, ring: Arc<TraceRing>, body: F) {
            use tracing_subscriber::prelude::*;
            let gate_arc = Arc::new(TraceGate::new());
            gate_arc.set(gate);
            let layer = TraceLayer::new(ring, gate_arc);
            let subscriber = tracing_subscriber::registry().with(layer);
            let dispatch = tracing::Dispatch::new(subscriber);
            tracing::dispatcher::with_default(&dispatch, body);
        }

        #[test]
        fn trace_layer_captures_event_with_message_and_typed_fields() {
            let ring = Arc::new(TraceRing::new(TRACE_RING_CAP));
            run_with_layer(GateLevel::Trace, ring.clone(), || {
                tracing::info!(sent = 1u32, channel = 44u32, "injected awdl announce frame");
            });

            let captured = ring.last_n(10);
            assert_eq!(captured.len(), 1);
            let ev = &captured[0];
            assert_eq!(ev.level, "INFO");
            assert!(
                ev.target.starts_with("filin_rs::introspect"),
                "target was {}",
                ev.target
            );
            assert_eq!(ev.msg, "injected awdl announce frame");
            assert_eq!(ev.fields.get("sent").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(ev.fields.get("channel").and_then(|v| v.as_u64()), Some(44));
            // message must NOT also appear in `fields` — it has its own slot.
            assert!(ev.fields.get("message").is_none());
            assert!(ev.ts > 0);
        }

        #[test]
        fn trace_layer_captures_string_and_bool_fields_typed() {
            let ring = Arc::new(TraceRing::new(TRACE_RING_CAP));
            run_with_layer(GateLevel::Trace, ring.clone(), || {
                tracing::warn!(iface = "wlan0mon", ok = true, "channel switch failed");
            });

            let ev = &ring.last_n(1)[0];
            assert_eq!(ev.level, "WARN");
            assert_eq!(ev.msg, "channel switch failed");
            assert_eq!(ev.fields.get("iface").and_then(|v| v.as_str()), Some("wlan0mon"));
            assert_eq!(ev.fields.get("ok").and_then(|v| v.as_bool()), Some(true));
        }

        #[test]
        fn trace_layer_off_by_default_captures_nothing() {
            let ring = Arc::new(TraceRing::new(TRACE_RING_CAP));
            run_with_layer(GateLevel::Off, ring.clone(), || {
                tracing::error!("must not be captured");
            });

            assert!(ring.is_empty());
        }

        #[test]
        fn trace_layer_debug_gate_skips_trace_events() {
            let ring = Arc::new(TraceRing::new(TRACE_RING_CAP));
            run_with_layer(GateLevel::Debug, ring.clone(), || {
                tracing::info!("info captured");
                tracing::trace!("trace skipped");
                tracing::debug!("debug captured");
            });

            let captured = ring.last_n(10);
            let msgs: Vec<&str> = captured.iter().map(|e| e.msg.as_str()).collect();
            assert_eq!(msgs, vec!["info captured", "debug captured"]);
        }

        fn req<'a>(method: &'a str, path: &'a str) -> Request<'a> {
            Request {
                method,
                path,
                query: "",
                body: &[],
            }
        }

        fn req_with_body<'a>(method: &'a str, path: &'a str, body: &'a [u8]) -> Request<'a> {
            Request {
                method,
                path,
                query: "",
                body,
            }
        }

        fn req_with_query<'a>(method: &'a str, path_and_query: &'a str) -> Request<'a> {
            // split "path?query" into path + query for the router
            let (path, query) = path_and_query
                .split_once('?')
                .unwrap_or((path_and_query, ""));
            Request {
                method,
                path,
                query,
                body: &[],
            }
        }

        fn json_body(resp: &Response) -> serde_json::Value {
            serde_json::from_slice(&resp.body).expect("response body is valid JSON")
        }

        #[test]
        fn status_route_merges_snapshot_and_counters() {
            let intro = Introspection::new();
            intro.set_status(StatusSnapshot {
                current_channel: 44,
                master_mac: mac_string([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
                master_seq: 9,
                synced: true,
                peer_count: 2,
                master_anchor: 0,
                tsf_offset_us: 800,
                master_age_ms: 3200,
                sync_addr_mac: mac_string([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
            });
            intro.counters.inc_mdns_rx_self();
            intro.counters.inc_mdns_rx_self();
            intro.counters.inc_mdns_rx_other();
            intro.counters.inc_rebroadcast(44);
            intro.counters.inc_rebroadcast(44);
            intro.counters.inc_rebroadcast(149);
            // sync-quality counters
            intro.counters.inc_master_change();
            intro.counters.inc_master_change();
            intro.counters.push_master_mac("02:aa:bb:cc:dd:ee".into());
            intro.counters.push_master_mac("02:00:00:00:00:99".into());
            intro.counters.record_aw_check(true);
            intro.counters.record_aw_check(true);
            intro.counters.record_aw_check(false);

            let resp = route(&intro, &req("GET", "/status"));
            assert_eq!(resp.status, 200);
            assert!(resp.content_type.contains("application/json"));
            let obj = json_body(&resp).as_object().unwrap().clone();
            assert_eq!(obj.get("current_channel").and_then(|v| v.as_u64()), Some(44));
            assert_eq!(
                obj.get("master_mac").and_then(|v| v.as_str()),
                Some("02:aa:bb:cc:dd:ee")
            );
            assert_eq!(obj.get("master_seq").and_then(|v| v.as_u64()), Some(9));
            assert_eq!(obj.get("synced").and_then(|v| v.as_bool()), Some(true));
            assert_eq!(obj.get("peer_count").and_then(|v| v.as_u64()), Some(2));
            assert_eq!(obj.get("mdns_rx_self").and_then(|v| v.as_u64()), Some(2));
            assert_eq!(obj.get("mdns_rx_other").and_then(|v| v.as_u64()), Some(1));
            let rbc = obj.get("rebroadcast_counts").unwrap().as_object().unwrap();
            // Adaptive map: only channels that were bumped appear.
            assert!(!rbc.contains_key("6"), "ch6 never bumped");
            assert_eq!(rbc.get("44").and_then(|v| v.as_u64()), Some(2));
            assert_eq!(rbc.get("149").and_then(|v| v.as_u64()), Some(1));
            // Snapshot-backed sync-quality fields.
            assert_eq!(
                obj.get("tsf_offset_us").and_then(|v| v.as_u64()),
                Some(800),
                "/status must expose tsf_offset_us"
            );
            assert_eq!(
                obj.get("master_age_ms").and_then(|v| v.as_u64()),
                Some(3200),
                "/status must expose master_age_ms"
            );
            // Counter-backed sync-quality fields.
            assert_eq!(
                obj.get("master_changes").and_then(|v| v.as_u64()),
                Some(2),
                "/status must expose master_changes"
            );
            assert_eq!(
                obj.get("aw_alignment_pct").and_then(|v| v.as_u64()),
                Some(66),
                "/status must expose aw_alignment_pct (2/3 aligned = 66%)"
            );
            let macs = obj
                .get("last_master_macs")
                .expect("/status must expose last_master_macs")
                .as_array()
                .unwrap();
            assert_eq!(macs.len(), 2);
            assert_eq!(macs[0].as_str(), Some("02:aa:bb:cc:dd:ee"));
            assert_eq!(macs[1].as_str(), Some("02:00:00:00:00:99"));
        }

        #[test]
        fn peers_route_returns_array_of_peer_views() {
            let intro = Introspection::new();
            intro.set_peers(vec![
                PeerView {
                    mac: mac_string([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
                    last_seen_ms_ago: 312,
                    decoded_chanseq: vec![44, 44, 6, 6, 149, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44],
                    current_channel: 44,
                },
            ]);

            let resp = route(&intro, &req("GET", "/peers"));
            assert_eq!(resp.status, 200);
            let arr = json_body(&resp).as_array().unwrap().clone();
            assert_eq!(arr.len(), 1);
            let p = arr[0].as_object().unwrap();
            assert_eq!(
                p.get("mac").and_then(|v| v.as_str()),
                Some("02:aa:bb:cc:dd:ee")
            );
            assert_eq!(p.get("last_seen_ms_ago").and_then(|v| v.as_u64()), Some(312));
            let seq = p.get("decoded_chanseq").unwrap().as_array().unwrap();
            assert_eq!(seq.len(), 16);
            assert_eq!(seq[2].as_u64(), Some(6));
            assert_eq!(seq[4].as_u64(), Some(149));
            assert_eq!(p.get("current_channel").and_then(|v| v.as_u64()), Some(44));
        }

        #[test]
        fn trace_route_returns_last_n_in_insertion_order() {
            let ring = Arc::new(TraceRing::new(TRACE_RING_CAP));
            let gate = Arc::new(TraceGate::new());
            gate.set(GateLevel::Trace);
            let layer = TraceLayer::new(ring.clone(), gate);
            use tracing_subscriber::prelude::*;
            let dispatch =
                tracing::Dispatch::new(tracing_subscriber::registry().with(layer));
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::info!("first");
                tracing::info!("second");
                tracing::info!("third");
            });

            let intro = Introspection::new_with_trace(ring, Arc::new(TraceGate::new()));
            let resp = route(&intro, &req_with_query("GET", "/trace?n=2"));
            assert_eq!(resp.status, 200);
            let body = json_body(&resp);
            let arr = body.as_array().unwrap();
            assert_eq!(arr.len(), 2);
            assert_eq!(
                arr[0].as_object().unwrap().get("msg").and_then(|v| v.as_str()),
                Some("second")
            );
            assert_eq!(
                arr[1].as_object().unwrap().get("msg").and_then(|v| v.as_str()),
                Some("third")
            );
        }

        #[test]
        fn trace_post_sets_gate_for_valid_level() {
            let intro = Introspection::new();
            // default off
            assert!(!intro.trace_gate().allows(3));

            let resp = route(
                &intro,
                &req_with_body("POST", "/trace", br#"{"level":"trace"}"#),
            );
            assert_eq!(resp.status, 200);
            assert!(intro.trace_gate().allows(5));

            let resp = route(
                &intro,
                &req_with_body("POST", "/trace", br#"{"level":"off"}"#),
            );
            assert_eq!(resp.status, 200);
            assert!(!intro.trace_gate().allows(1));
        }

        #[test]
        fn trace_post_rejects_unknown_level_or_bad_json_with_400() {
            let intro = Introspection::new();

            let resp = route(
                &intro,
                &req_with_body("POST", "/trace", br#"{"level":"verbose"}"#),
            );
            assert_eq!(resp.status, 400);

            let resp = route(
                &intro,
                &req_with_body("POST", "/trace", b"not json at all"),
            );
            assert_eq!(resp.status, 400);
        }

        #[test]
        fn unknown_route_and_wrong_method_both_return_404() {
            let intro = Introspection::new();

            assert_eq!(route(&intro, &req("GET", "/nope")).status, 404);
            assert_eq!(route(&intro, &req("POST", "/status")).status, 404);
            assert_eq!(route(&intro, &req("DELETE", "/trace")).status, 404);
        }

        #[test]
        fn parse_request_line_extracts_method_path_query() {
            let p = parse_request_line("GET /trace?n=300 HTTP/1.1\r\n").expect("valid line");
            assert_eq!(p.method, "GET");
            assert_eq!(p.path, "/trace");
            assert_eq!(p.query, "n=300");

            let p = parse_request_line("POST /trace HTTP/1.1\r\n").expect("valid line");
            assert_eq!(p.method, "POST");
            assert_eq!(p.path, "/trace");
            assert_eq!(p.query, "");

            assert!(parse_request_line("garbage").is_none());
            assert!(parse_request_line("").is_none());
        }

        #[test]
        fn format_response_writes_status_headers_and_body() {
            let resp = Response {
                status: 200,
                content_type: "application/json",
                body: b"{\"ok\":1}".to_vec(),
            };
            let bytes = format_response(&resp);
            let text = String::from_utf8_lossy(&bytes);

            assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(text.contains("Content-Type: application/json\r\n"));
            assert!(text.contains("Content-Length: 8\r\n"));
            assert!(text.contains("Connection: close\r\n"));
            assert!(text.ends_with("{\"ok\":1}"));
        }

        #[test]
        fn format_response_uses_correct_reason_phrase_for_errors() {
            let not_found = Response {
                status: 404,
                content_type: "text/plain; charset=utf-8",
                body: b"not found".to_vec(),
            };
            let bad = Response {
                status: 400,
                content_type: "text/plain; charset=utf-8",
                body: b"bad".to_vec(),
            };
            assert!(String::from_utf8(format_response(&not_found)).unwrap().contains("404 Not Found"));
            assert!(String::from_utf8(format_response(&bad)).unwrap().contains("400 Bad Request"));
        }

        #[test]
        fn spawn_serves_endpoints_over_real_loopback_tcp() {
            use std::io::{Read, Write};
            use std::net::{SocketAddr, TcpStream};

            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
            let addr = listener.local_addr().expect("local addr");
            let intro = Introspection::new();
            intro.set_status(StatusSnapshot {
                current_channel: 44,
                master_mac: "02:aa:bb:cc:dd:ee".into(),
                master_seq: 5,
                synced: true,
                peer_count: 1,
                master_anchor: 0,
                tsf_offset_us: 0,
                master_age_ms: 0,
                sync_addr_mac: "02:aa:bb:cc:dd:ee".into(),
            });
            intro.counters.inc_mdns_rx_other();
            intro.counters.inc_rebroadcast(44);
            let _join = spawn(intro.clone(), listener);

            // GET /status
            let mut s = TcpStream::connect(addr).expect("connect");
            s.write_all(b"GET /status HTTP/1.0\r\n\r\n").expect("write");
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).expect("read");
            let status = String::from_utf8_lossy(&buf).into_owned();
            let body = status.split("\r\n\r\n").nth(1).unwrap_or("");
            assert!(status.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(body.contains("\"current_channel\":44"));
            assert!(body.contains("\"master_mac\":\"02:aa:bb:cc:dd:ee\""));
            assert!(body.contains("\"mdns_rx_other\":1"));
            assert!(body.contains("\"rebroadcast_counts\""));
            assert!(body.contains("\"44\":1"));

            // GET /peers (empty array — no peers set)
            let mut s = TcpStream::connect(addr).expect("connect");
            s.write_all(b"GET /peers HTTP/1.0\r\n\r\n").expect("write");
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).expect("read");
            let peers = String::from_utf8_lossy(&buf);
            assert!(peers.split("\r\n\r\n").nth(1).unwrap_or("").contains("[]"));

            // POST /trace {"level":"trace"}
            assert!(!intro.trace_gate().allows(5));
            let mut s = TcpStream::connect(addr).expect("connect");
            write!(
                s,
                "POST /trace HTTP/1.0\r\nContent-Length: {}\r\n\r\n",
                b"{\"level\":\"trace\"}".len()
            )
            .expect("write headers");
            s.write_all(b"{\"level\":\"trace\"}").expect("write body");
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).expect("read");
            let post = String::from_utf8_lossy(&buf);
            assert!(post.starts_with("HTTP/1.1 200"));
            assert!(intro.trace_gate().allows(5));

            // 404 path
            let mut s = TcpStream::connect(addr).expect("connect");
            s.write_all(b"GET /nope HTTP/1.0\r\n\r\n").expect("write");
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).expect("read");
            let notfound = String::from_utf8_lossy(&buf);
            assert!(notfound.starts_with("HTTP/1.1 404"));

            // silence unused import warning when SocketAddr differs by platform
            let _: SocketAddr = addr;
        }
    }
}
