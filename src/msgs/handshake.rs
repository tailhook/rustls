use msgs::enums::{ProtocolVersion, HandshakeType};
use msgs::enums::{CipherSuite, Compression, ExtensionType, ECPointFormat};
use msgs::enums::{HashAlgorithm, SignatureAlgorithm, HeartbeatMode, ServerNameType};
use msgs::enums::{SignatureScheme, KeyUpdateRequest, NamedGroup};
use msgs::enums::ClientCertificateType;
use msgs::enums::ECCurveType;
use msgs::base::{Payload, PayloadU8, PayloadU16, PayloadU24};
use msgs::codec;
use msgs::codec::{Codec, Reader};

use std::io::Write;
use std::collections;

macro_rules! declare_u8_vec(
  ($name:ident, $itemtype:ty) => {
    pub type $name = Vec<$itemtype>;

    impl Codec for $name {
      fn encode(&self, bytes: &mut Vec<u8>) {
        codec::encode_vec_u8(bytes, self);
      }

      fn read(r: &mut Reader) -> Option<$name> {
        codec::read_vec_u8::<$itemtype>(r)
      }
    }
  }
);

macro_rules! declare_u16_vec(
  ($name:ident, $itemtype:ty) => {
    pub type $name = Vec<$itemtype>;

    impl Codec for $name {
      fn encode(&self, bytes: &mut Vec<u8>) {
        codec::encode_vec_u16(bytes, self);
      }

      fn read(r: &mut Reader) -> Option<$name> {
        codec::read_vec_u16::<$itemtype>(r)
      }
    }
  }
);

#[derive(Debug)]
pub struct Random {
  pub gmt_unix_time: u32,
  pub opaque: [u8; 28]
}

impl Codec for Random {
  fn encode(&self, bytes: &mut Vec<u8>) {
    codec::encode_u32(self.gmt_unix_time, bytes);
    bytes.extend_from_slice(&self.opaque);
  }

  fn read(r: &mut Reader) -> Option<Random> {
    let time = try_ret!(codec::read_u32(r));
    let bytes = try_ret!(r.take(28));
    let mut opaque = [0; 28];
    opaque.clone_from_slice(bytes);

    Some(Random { gmt_unix_time: time, opaque: opaque })
  }
}

impl Random {
  pub fn from_slice(bytes: &[u8]) -> Random {
    let mut rd = Reader::init(&bytes);
    Random::read(&mut rd).unwrap()
  }

  pub fn write_slice(&self, mut bytes: &mut [u8]) {
    let buf = self.get_encoding();
    bytes.write(&buf).unwrap();
  }
}

#[derive(Debug, PartialEq, Clone)]
pub struct SessionID {
  bytes: Vec<u8>
}

impl Codec for SessionID {
  fn encode(&self, bytes: &mut Vec<u8>) {
    debug_assert!(self.bytes.len() <= 32);
    bytes.push(self.bytes.len() as u8);
    bytes.extend_from_slice(&self.bytes);
  }

  fn read(r: &mut Reader) -> Option<SessionID> {
    let len = try_ret!(codec::read_u8(r));
    let bytes = try_ret!(r.take(len as usize));

    if len <= 32 {
      Some(SessionID { bytes: bytes.to_vec() })
    } else {
      None
    }
  }
}

impl SessionID {
  pub fn new(mut bytes: Vec<u8>) -> SessionID {
    bytes.truncate(32);
    SessionID { bytes: bytes }
  }

  pub fn empty() -> SessionID {
    SessionID::new(Vec::new())
  }

  pub fn len(&self) -> usize {
    return self.bytes.len()
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
}

#[derive(Debug)]
pub struct UnknownExtension {
  typ: ExtensionType,
  payload: Payload
}

impl UnknownExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.payload.encode(bytes);
  }

  fn read(typ: ExtensionType, r: &mut Reader) -> Option<UnknownExtension> {
    let payload = try_ret!(Payload::read(r));
    Some(UnknownExtension { typ: typ, payload: payload })
  }
}

declare_u8_vec!(ECPointFormatList, ECPointFormat);

pub trait SupportedPointFormats {
  fn supported() -> ECPointFormatList;
}

impl SupportedPointFormats for ECPointFormatList {
  fn supported() -> ECPointFormatList {
    vec![ECPointFormat::Uncompressed]
  }
}

declare_u16_vec!(NamedGroups, NamedGroup);

pub trait SupportedGroups {
  fn supported() -> NamedGroups;
}

impl SupportedGroups for NamedGroups {
  fn supported() -> NamedGroups {
    vec![ NamedGroup::X25519, NamedGroup::secp384r1, NamedGroup::secp256r1 ]
  }
}

declare_u16_vec!(SupportedSignatureSchemes, SignatureScheme);

pub trait DecomposedSignatureScheme {
  fn sign(&self) -> SignatureAlgorithm;
  fn hash(&self) -> HashAlgorithm;
  fn make(alg: SignatureAlgorithm, hash: HashAlgorithm) -> SignatureScheme;
}

impl DecomposedSignatureScheme for SignatureScheme {
  fn sign(&self) -> SignatureAlgorithm {
    match *self {
      SignatureScheme::RSA_PKCS1_SHA1 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PKCS1_SHA256 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PKCS1_SHA384 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PKCS1_SHA512 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PSS_SHA256 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PSS_SHA384 => SignatureAlgorithm::RSA,
      SignatureScheme::RSA_PSS_SHA512 => SignatureAlgorithm::RSA,
      SignatureScheme::ECDSA_NISTP256_SHA256 => SignatureAlgorithm::ECDSA,
      SignatureScheme::ECDSA_NISTP384_SHA384 => SignatureAlgorithm::ECDSA,
      SignatureScheme::ECDSA_NISTP521_SHA512 => SignatureAlgorithm::ECDSA,
      _ => SignatureAlgorithm::Unknown(0)
    }
  }

  fn hash(&self) -> HashAlgorithm {
    match *self {
      SignatureScheme::RSA_PKCS1_SHA1 => HashAlgorithm::SHA1,
      SignatureScheme::RSA_PKCS1_SHA256 => HashAlgorithm::SHA256,
      SignatureScheme::RSA_PKCS1_SHA384 => HashAlgorithm::SHA384,
      SignatureScheme::RSA_PKCS1_SHA512 => HashAlgorithm::SHA512,
      SignatureScheme::RSA_PSS_SHA256 => HashAlgorithm::SHA256,
      SignatureScheme::RSA_PSS_SHA384 => HashAlgorithm::SHA384,
      SignatureScheme::RSA_PSS_SHA512 => HashAlgorithm::SHA512,
      SignatureScheme::ECDSA_NISTP256_SHA256 => HashAlgorithm::SHA256,
      SignatureScheme::ECDSA_NISTP384_SHA384 => HashAlgorithm::SHA384,
      SignatureScheme::ECDSA_NISTP521_SHA512 => HashAlgorithm::SHA512,
      _ => HashAlgorithm::NONE
    }
  }

  fn make(alg: SignatureAlgorithm, hash: HashAlgorithm) -> SignatureScheme {
    use msgs::enums::SignatureAlgorithm::{RSA, ECDSA};
    use msgs::enums::HashAlgorithm::{SHA1, SHA256, SHA384, SHA512};

    match (alg, hash) {
      (RSA, SHA1) => SignatureScheme::RSA_PKCS1_SHA1,
      (RSA, SHA256) => SignatureScheme::RSA_PKCS1_SHA256,
      (RSA, SHA384) => SignatureScheme::RSA_PKCS1_SHA384,
      (RSA, SHA512) => SignatureScheme::RSA_PKCS1_SHA512,
      (ECDSA, SHA256) => SignatureScheme::ECDSA_NISTP256_SHA256,
      (ECDSA, SHA384) => SignatureScheme::ECDSA_NISTP384_SHA384,
      (ECDSA, SHA512) => SignatureScheme::ECDSA_NISTP521_SHA512,
      (_, _) => unreachable!()
    }
  }
}

pub trait SupportedMandatedSignatureSchemes {
  fn mandated() -> SupportedSignatureSchemes;
  fn supported_verify() -> SupportedSignatureSchemes;
}

impl SupportedMandatedSignatureSchemes for SupportedSignatureSchemes {
  /// What SupportedSignatureSchemes are hardcoded in the TLS1.2 RFC.
  /// Yes, you cannot avoid SHA1 in standard TLS.
  fn mandated() -> SupportedSignatureSchemes {
    vec![
      SignatureScheme::RSA_PKCS1_SHA1,
    ]
  }

  /// Supported signature verification algorithms in decreasing order of expected security.
  fn supported_verify() -> SupportedSignatureSchemes {
    vec![
      /* FIXME: ed448 */
      SignatureScheme::ED25519,

      /* FIXME: ECDSA-P521-SHA512 */
      SignatureScheme::ECDSA_NISTP384_SHA384,
      SignatureScheme::ECDSA_NISTP256_SHA256,

      /* FIXME: PSS is a lie! */
      SignatureScheme::RSA_PSS_SHA512,
      SignatureScheme::RSA_PSS_SHA384,
      SignatureScheme::RSA_PSS_SHA256,

      SignatureScheme::RSA_PKCS1_SHA512,
      SignatureScheme::RSA_PKCS1_SHA384,
      SignatureScheme::RSA_PKCS1_SHA256,

      SignatureScheme::RSA_PKCS1_SHA1,
    ]
  }
}

#[derive(Debug)]
pub enum ServerNamePayload {
  HostName(String),
  Unknown(Payload)
}

impl ServerNamePayload {
  fn read_hostname(r: &mut Reader) -> Option<ServerNamePayload> {
    let len = try_ret!(codec::read_u16(r)) as usize;
    let name = try_ret!(r.take(len));
    let hostname = String::from_utf8(name.to_vec());

    match hostname {
      Ok(n) => Some(ServerNamePayload::HostName(n)),
      _ => None
    }
  }

  fn encode_hostname(name: &String, bytes: &mut Vec<u8>) {
    codec::encode_u16(name.len() as u16, bytes);
    bytes.extend_from_slice(name.as_bytes());
  }

  fn encode(&self, bytes: &mut Vec<u8>) {
    match *self {
      ServerNamePayload::HostName(ref r) => ServerNamePayload::encode_hostname(r, bytes),
      ServerNamePayload::Unknown(ref r) => r.encode(bytes)
    }
  }
}

#[derive(Debug)]
pub struct ServerName {
  pub typ: ServerNameType,
  pub payload: ServerNamePayload
}

impl Codec for ServerName {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.typ.encode(bytes);
    self.payload.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<ServerName> {
    let typ = try_ret!(ServerNameType::read(r));

    let payload = match typ {
      ServerNameType::HostName =>
        try_ret!(ServerNamePayload::read_hostname(r)),
      _ =>
        ServerNamePayload::Unknown(try_ret!(Payload::read(r)))
    };

    Some(ServerName { typ: typ, payload: payload })
  }
}

declare_u16_vec!(ServerNameRequest, ServerName);

pub type ProtocolName = PayloadU8;
declare_u16_vec!(ProtocolNameList, ProtocolName);

pub trait ConvertProtocolNameList {
  fn from_strings(names: &[String]) -> Self;
  fn to_strings(&self) -> Vec<String>;
  fn to_single_string(&self) -> Option<String>;
}

impl ConvertProtocolNameList for ProtocolNameList {
  fn from_strings(names: &[String]) -> ProtocolNameList {
    let mut ret = Vec::new();

    for name in names {
      ret.push(PayloadU8::new(name.as_bytes().to_vec()));
    }

    ret
  }

  fn to_strings(&self) -> Vec<String> {
    let mut ret = Vec::new();
    for proto in self {
      match String::from_utf8(proto.0.clone()).ok() {
        Some(st) => ret.push(st),
        _ => {}
      }
    }
    ret
  }

  fn to_single_string(&self) -> Option<String> {
    if self.len() == 1 {
      String::from_utf8(self[0].0.clone()).ok()
    } else {
      None
    }
  }
}

/* --- TLS 1.3 Key shares --- */
#[derive(Debug)]
pub struct KeyShareEntry {
  pub group: NamedGroup,
  pub payload: PayloadU16
}

impl KeyShareEntry {
  pub fn new(group: NamedGroup, payload: &[u8]) -> KeyShareEntry {
    KeyShareEntry {
      group: group,
      payload: PayloadU16::new(payload.to_vec())
    }
  }
}

impl Codec for KeyShareEntry {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.group.encode(bytes);
    self.payload.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<KeyShareEntry> {
    let group = try_ret!(NamedGroup::read(r));
    let payload = try_ret!(PayloadU16::read(r));

    Some(KeyShareEntry {
      group: group,
      payload: payload
    })
  }
}

declare_u16_vec!(KeyShareEntries, KeyShareEntry);

declare_u8_vec!(ProtocolVersions, ProtocolVersion);

#[derive(Debug)]
pub enum ClientExtension {
  ECPointFormats(ECPointFormatList),
  NamedGroups(NamedGroups),
  SignatureAlgorithms(SupportedSignatureSchemes),
  Heartbeat(HeartbeatMode),
  ServerName(ServerNameRequest),
  SessionTicketRequest,
  SessionTicketOffer(Payload),
  Protocols(ProtocolNameList),
  SupportedVersions(ProtocolVersions),
  KeyShare(KeyShareEntries),
  Unknown(UnknownExtension)
}

impl ClientExtension {
  pub fn get_type(&self) -> ExtensionType {
    match *self {
      ClientExtension::ECPointFormats(_) => ExtensionType::ECPointFormats,
      ClientExtension::NamedGroups(_) => ExtensionType::EllipticCurves,
      ClientExtension::SignatureAlgorithms(_) => ExtensionType::SignatureAlgorithms,
      ClientExtension::Heartbeat(_) => ExtensionType::Heartbeat,
      ClientExtension::ServerName(_) => ExtensionType::ServerName,
      ClientExtension::SessionTicketRequest => ExtensionType::SessionTicket,
      ClientExtension::SessionTicketOffer(_) => ExtensionType::SessionTicket,
      ClientExtension::Protocols(_) => ExtensionType::ALProtocolNegotiation,
      ClientExtension::SupportedVersions(_) => ExtensionType::SupportedVersions,
      ClientExtension::KeyShare(_) => ExtensionType::KeyShare,
      ClientExtension::Unknown(ref r) => r.typ
    }
  }
}

impl Codec for ClientExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.get_type().encode(bytes);

    let mut sub: Vec<u8> = Vec::new();
    match *self {
      ClientExtension::ECPointFormats(ref r) => r.encode(&mut sub),
      ClientExtension::NamedGroups(ref r) => r.encode(&mut sub),
      ClientExtension::SignatureAlgorithms(ref r) => r.encode(&mut sub),
      ClientExtension::Heartbeat(ref r) => r.encode(&mut sub),
      ClientExtension::ServerName(ref r) => r.encode(&mut sub),
      ClientExtension::SessionTicketRequest => (),
      ClientExtension::SessionTicketOffer(ref r) => r.encode(&mut sub),
      ClientExtension::Protocols(ref r) => r.encode(&mut sub),
      ClientExtension::SupportedVersions(ref r) => r.encode(&mut sub),
      ClientExtension::KeyShare(ref r) => r.encode(&mut sub),
      ClientExtension::Unknown(ref r) => r.encode(&mut sub)
    }

    codec::encode_u16(sub.len() as u16, bytes);
    bytes.append(&mut sub);
  }

  fn read(r: &mut Reader) -> Option<ClientExtension> {
    let typ = try_ret!(ExtensionType::read(r));
    let len = try_ret!(codec::read_u16(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    Some(match typ {
      ExtensionType::ECPointFormats =>
        ClientExtension::ECPointFormats(try_ret!(ECPointFormatList::read(&mut sub))),
      ExtensionType::EllipticCurves =>
        ClientExtension::NamedGroups(try_ret!(NamedGroups::read(&mut sub))),
      ExtensionType::SignatureAlgorithms =>
        ClientExtension::SignatureAlgorithms(try_ret!(SupportedSignatureSchemes::read(&mut sub))),
      ExtensionType::Heartbeat =>
        ClientExtension::Heartbeat(try_ret!(HeartbeatMode::read(&mut sub))),
      ExtensionType::ServerName =>
        ClientExtension::ServerName(try_ret!(ServerNameRequest::read(&mut sub))),
      ExtensionType::SessionTicket =>
        if sub.any_left() {
          ClientExtension::SessionTicketOffer(try_ret!(Payload::read(&mut sub)))
        } else {
          ClientExtension::SessionTicketRequest
        },
      ExtensionType::ALProtocolNegotiation =>
        ClientExtension::Protocols(try_ret!(ProtocolNameList::read(&mut sub))),
      ExtensionType::SupportedVersions =>
        ClientExtension::SupportedVersions(try_ret!(ProtocolVersions::read(&mut sub))),
      ExtensionType::KeyShare =>
        ClientExtension::KeyShare(try_ret!(KeyShareEntries::read(&mut sub))),
      _ =>
        ClientExtension::Unknown(try_ret!(UnknownExtension::read(typ, &mut sub)))
    })
  }
}

impl ClientExtension {
  /// Make a basic SNI ServerNameRequest quoting `hostname`.
  pub fn make_sni(hostname: &str) -> ClientExtension {
    let name = ServerName {
      typ: ServerNameType::HostName,
      payload: ServerNamePayload::HostName(hostname.to_string())
    };

    ClientExtension::ServerName(
      vec![ name ]
    )
  }
}

#[derive(Debug)]
pub enum ServerExtension {
  ECPointFormats(ECPointFormatList),
  Heartbeat(HeartbeatMode),
  ServerNameAcknowledgement,
  SessionTicketAcknowledgement,
  RenegotiationInfo(PayloadU8),
  Protocols(ProtocolNameList),
  KeyShare(KeyShareEntry),
  Unknown(UnknownExtension)
}

impl ServerExtension {
  pub fn get_type(&self) -> ExtensionType {
    match *self {
      ServerExtension::ECPointFormats(_) => ExtensionType::ECPointFormats,
      ServerExtension::Heartbeat(_) => ExtensionType::Heartbeat,
      ServerExtension::ServerNameAcknowledgement => ExtensionType::ServerName,
      ServerExtension::SessionTicketAcknowledgement => ExtensionType::SessionTicket,
      ServerExtension::RenegotiationInfo(_) => ExtensionType::RenegotiationInfo,
      ServerExtension::Protocols(_) => ExtensionType::ALProtocolNegotiation,
      ServerExtension::KeyShare(_) => ExtensionType::KeyShare,
      ServerExtension::Unknown(ref r) => r.typ
    }
  }
}

impl Codec for ServerExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.get_type().encode(bytes);

    let mut sub: Vec<u8> = Vec::new();
    match *self {
      ServerExtension::ECPointFormats(ref r) => r.encode(&mut sub),
      ServerExtension::Heartbeat(ref r) => r.encode(&mut sub),
      ServerExtension::ServerNameAcknowledgement => (),
      ServerExtension::SessionTicketAcknowledgement => (),
      ServerExtension::RenegotiationInfo(ref r) => r.encode(&mut sub),
      ServerExtension::Protocols(ref r) => r.encode(&mut sub),
      ServerExtension::KeyShare(ref r) => r.encode(&mut sub),
      ServerExtension::Unknown(ref r) => r.encode(&mut sub)
    }

    codec::encode_u16(sub.len() as u16, bytes);
    bytes.append(&mut sub);
  }

  fn read(r: &mut Reader) -> Option<ServerExtension> {
    let typ = try_ret!(ExtensionType::read(r));
    let len = try_ret!(codec::read_u16(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    Some(match typ {
      ExtensionType::ECPointFormats =>
        ServerExtension::ECPointFormats(try_ret!(ECPointFormatList::read(&mut sub))),
      ExtensionType::Heartbeat =>
        ServerExtension::Heartbeat(try_ret!(HeartbeatMode::read(&mut sub))),
      ExtensionType::ServerName =>
        ServerExtension::ServerNameAcknowledgement,
      ExtensionType::SessionTicket =>
        ServerExtension::SessionTicketAcknowledgement,
      ExtensionType::RenegotiationInfo =>
        ServerExtension::RenegotiationInfo(try_ret!(PayloadU8::read(&mut sub))),
      ExtensionType::ALProtocolNegotiation =>
        ServerExtension::Protocols(try_ret!(ProtocolNameList::read(&mut sub))),
      ExtensionType::KeyShare =>
        ServerExtension::KeyShare(try_ret!(KeyShareEntry::read(&mut sub))),
      _ =>
        ServerExtension::Unknown(try_ret!(UnknownExtension::read(typ, &mut sub)))
    })
  }
}

impl ServerExtension {
  pub fn make_alpn(proto: String) -> ServerExtension {
    ServerExtension::Protocols(ProtocolNameList::from_strings(&[proto]))
  }

  pub fn make_empty_renegotiation_info() -> ServerExtension {
    let empty = Vec::new();
    ServerExtension::RenegotiationInfo(PayloadU8::new(empty))
  }
}

#[derive(Debug)]
pub struct ClientHelloPayload {
  pub client_version: ProtocolVersion,
  pub random: Random,
  pub session_id: SessionID,
  pub cipher_suites: Vec<CipherSuite>,
  pub compression_methods: Vec<Compression>,
  pub extensions: Vec<ClientExtension>
}

impl Codec for ClientHelloPayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.client_version.encode(bytes);
    self.random.encode(bytes);
    self.session_id.encode(bytes);
    codec::encode_vec_u16(bytes, &self.cipher_suites);
    codec::encode_vec_u8(bytes, &self.compression_methods);

    if self.extensions.len() > 0 {
      codec::encode_vec_u16(bytes, &self.extensions);
    }
  }

  fn read(r: &mut Reader) -> Option<ClientHelloPayload> {

    let mut ret = ClientHelloPayload {
      client_version: try_ret!(ProtocolVersion::read(r)),
      random: try_ret!(Random::read(r)),
      session_id: try_ret!(SessionID::read(r)),
      cipher_suites: try_ret!(codec::read_vec_u16::<CipherSuite>(r)),
      compression_methods: try_ret!(codec::read_vec_u8::<Compression>(r)),
      extensions: Vec::new()
    };

    if r.any_left() {
      ret.extensions = try_ret!(codec::read_vec_u16::<ClientExtension>(r));
    }

    Some(ret)
  }
}

impl ClientHelloPayload {
  /// Returns true if there is more than one extension of a given
  /// type.
  pub fn has_duplicate_extension(&self) -> bool {
    let mut seen = collections::HashSet::new();

    for ext in &self.extensions {
      let typ = ext.get_type().get_u16();

      if seen.contains(&typ) {
        return true;
      }
      seen.insert(typ);
    }

    false
  }

  pub fn find_extension(&self, ext: ExtensionType) -> Option<&ClientExtension> {
    self.extensions.iter().find(|x| x.get_type() == ext)
  }

  pub fn get_sni_extension(&self) -> Option<&ServerNameRequest> {
    let ext = try_ret!(self.find_extension(ExtensionType::ServerName));
    match *ext {
      ClientExtension::ServerName(ref req) => Some(req),
      _ => None
    }
  }

  pub fn get_sigalgs_extension(&self) -> Option<&SupportedSignatureSchemes> {
    let ext = try_ret!(self.find_extension(ExtensionType::SignatureAlgorithms));
    match *ext {
      ClientExtension::SignatureAlgorithms(ref req) => Some(req),
      _ => None
    }
  }

  pub fn get_namedgroups_extension(&self) -> Option<&NamedGroups> {
    let ext = try_ret!(self.find_extension(ExtensionType::EllipticCurves));
    match *ext {
      ClientExtension::NamedGroups(ref req) => Some(req),
      _ => None
    }
  }

  pub fn get_ecpoints_extension(&self) -> Option<&ECPointFormatList> {
    let ext = try_ret!(self.find_extension(ExtensionType::ECPointFormats));
    match *ext {
      ClientExtension::ECPointFormats(ref req) => Some(req),
      _ => None
    }
  }

  pub fn get_alpn_extension(&self) -> Option<&ProtocolNameList> {
    let ext = try_ret!(self.find_extension(ExtensionType::ALProtocolNegotiation));
    match *ext {
      ClientExtension::Protocols(ref req) => Some(req),
      _ => None
    }
  }

  pub fn get_ticket_extension(&self) -> Option<&ClientExtension> {
    self.find_extension(ExtensionType::SessionTicket)
  }
}

#[derive(Debug)]
pub enum HelloRetryExtension {
  KeyShare(NamedGroup),
  Cookie(PayloadU16),
  Unknown(UnknownExtension)
}

impl HelloRetryExtension {
  pub fn get_type(&self) -> ExtensionType {
    match *self {
      HelloRetryExtension::KeyShare(_) => ExtensionType::KeyShare,
      HelloRetryExtension::Cookie(_) => ExtensionType::Cookie,
      HelloRetryExtension::Unknown(ref r) => r.typ
    }
  }
}

impl Codec for HelloRetryExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.get_type().encode(bytes);

    let mut sub: Vec<u8> = Vec::new();
    match *self {
      HelloRetryExtension::KeyShare(ref r) => r.encode(&mut sub),
      HelloRetryExtension::Cookie(ref r) => r.encode(&mut sub),
      HelloRetryExtension::Unknown(ref r) => r.encode(&mut sub)
    }

    codec::encode_u16(sub.len() as u16, bytes);
  }

  fn read(r: &mut Reader) -> Option<HelloRetryExtension> {
    let typ = try_ret!(ExtensionType::read(r));
    let len = try_ret!(codec::read_u16(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    Some(match typ {
      ExtensionType::KeyShare =>
        HelloRetryExtension::KeyShare(try_ret!(NamedGroup::read(&mut sub))),
      ExtensionType::Heartbeat =>
        HelloRetryExtension::Cookie(try_ret!(PayloadU16::read(&mut sub))),
      _ =>
        HelloRetryExtension::Unknown(try_ret!(UnknownExtension::read(typ, &mut sub)))
    })
  }
}

#[derive(Debug)]
pub struct HelloRetryRequest {
  pub server_version: ProtocolVersion,
  pub extensions: Vec<HelloRetryExtension>
}

impl Codec for HelloRetryRequest {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.server_version.encode(bytes);
    codec::encode_vec_u16(bytes, &self.extensions);
  }

  fn read(r: &mut Reader) -> Option<HelloRetryRequest> {
    Some(HelloRetryRequest {
      server_version: try_ret!(ProtocolVersion::read(r)),
      extensions: try_ret!(codec::read_vec_u16::<HelloRetryExtension>(r))
    })
  }
}

#[derive(Debug)]
pub struct ServerHelloPayload {
  pub server_version: ProtocolVersion,
  pub random: Random,
  pub session_id: SessionID,
  pub cipher_suite: CipherSuite,
  pub compression_method: Compression,
  pub extensions: Vec<ServerExtension>
}

fn is_tls13(vers: ProtocolVersion) -> bool {
  vers == ProtocolVersion::TLSv1_3 ||
    vers == ProtocolVersion::Unknown(0x7f12)
}

impl Codec for ServerHelloPayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.server_version.encode(bytes);
    self.random.encode(bytes);

    if is_tls13(self.server_version) {
      self.cipher_suite.encode(bytes);
    } else {
      self.session_id.encode(bytes);
      self.cipher_suite.encode(bytes);
      self.compression_method.encode(bytes);
    }

    if !self.extensions.is_empty() {
      codec::encode_vec_u16(bytes, &self.extensions);
    }
  }

  fn read(r: &mut Reader) -> Option<ServerHelloPayload> {
    let version = try_ret!(ProtocolVersion::read(r));
    let random = try_ret!(Random::read(r));

    let (session_id, suite, compression) = if is_tls13(version) {
      (SessionID::empty(),
       try_ret!(CipherSuite::read(r)),
       Compression::Null)
    } else {
      (try_ret!(SessionID::read(r)),
       try_ret!(CipherSuite::read(r)),
       try_ret!(Compression::read(r)))
    };

    let mut ret = ServerHelloPayload {
      server_version: version,
      random: random,
      session_id: session_id,
      cipher_suite: suite,
      compression_method: compression,
      extensions: Vec::new()
    };

    if r.any_left() {
      ret.extensions = try_ret!(codec::read_vec_u16::<ServerExtension>(r));
    }

    Some(ret)
  }
}

impl ServerHelloPayload {
  /// Returns true if there is more than one extension of a given
  /// type.
  pub fn has_duplicate_extension(&self) -> bool {
    let mut seen = collections::HashSet::new();

    for ext in &self.extensions {
      let typ = ext.get_type().get_u16();

      if seen.contains(&typ) {
        return true;
      }
      seen.insert(typ);
    }

    false
  }

  pub fn find_extension(&self, ext: ExtensionType) -> Option<&ServerExtension> {
    self.extensions.iter().find(|x| x.get_type() == ext)
  }

  pub fn get_alpn_protocol(&self) -> Option<String> {
    let ext = try_ret!(self.find_extension(ExtensionType::ALProtocolNegotiation));
    match *ext {
      ServerExtension::Protocols(ref protos) => protos.to_single_string(),
      _ => None
    }
  }

  pub fn get_key_share(&self) -> Option<&KeyShareEntry> {
    let ext = try_ret!(self.find_extension(ExtensionType::KeyShare));
    match *ext {
      ServerExtension::KeyShare(ref share) => Some(share),
      _ => None
    }
  }
}

pub type ASN1Cert = PayloadU24;
pub type CertificatePayload = Vec<ASN1Cert>;

impl Codec for CertificatePayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    codec::encode_vec_u24(bytes, self);
  }

  fn read(r: &mut Reader) -> Option<CertificatePayload> {
    codec::read_vec_u24::<ASN1Cert>(r)
  }
}

/* TLS1.3 changes the Certificate payload encoding.
 * That's annoying. It means the parsing is not
 * context-free any more. */

#[derive(Debug)]
pub enum CertificateExtension {
  Unknown(UnknownExtension)
}

impl CertificateExtension {
  pub fn get_type(&self) -> ExtensionType {
    match *self {
      CertificateExtension::Unknown(ref r) => r.typ
    }
  }
}

impl Codec for CertificateExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.get_type().encode(bytes);

    let mut sub: Vec<u8> = Vec::new();
    match *self {
      CertificateExtension::Unknown(ref r) => r.encode(&mut sub)
    }

    codec::encode_u16(sub.len() as u16, bytes);
    bytes.append(&mut sub);
  }

  fn read(r: &mut Reader) -> Option<CertificateExtension> {
    let typ = try_ret!(ExtensionType::read(r));
    let len = try_ret!(codec::read_u16(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    Some(match typ {
      _ =>
        CertificateExtension::Unknown(try_ret!(UnknownExtension::read(typ, &mut sub)))
    })
  }
}

declare_u16_vec!(CertificateExtensions, CertificateExtension);

#[derive(Debug)]
pub struct CertificateEntry {
  pub cert: ASN1Cert,
  pub exts: CertificateExtensions
}

impl Codec for CertificateEntry {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.cert.encode(bytes);
    self.exts.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<CertificateEntry> {
    Some(CertificateEntry {
      cert: try_ret!(ASN1Cert::read(r)),
      exts: try_ret!(CertificateExtensions::read(r))
    })
  }
}

#[derive(Debug)]
pub struct CertificatePayloadTLS13 {
  pub request_context: PayloadU8,
  pub list: Vec<CertificateEntry>
}

impl Codec for CertificatePayloadTLS13 {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.request_context.encode(bytes);
    codec::encode_vec_u24(bytes, &self.list);
  }

  fn read(r: &mut Reader) -> Option<CertificatePayloadTLS13> {
    Some(CertificatePayloadTLS13 {
      request_context: try_ret!(PayloadU8::read(r)),
      list: try_ret!(codec::read_vec_u24::<CertificateEntry>(r))
    })
  }
}

impl CertificatePayloadTLS13 {
  pub fn convert(&self) -> CertificatePayload {
    let mut ret = Vec::new();
    for entry in &self.list {
      ret.push(entry.cert.clone());
    }
    ret
  }
}

#[derive(Debug)]
pub enum KeyExchangeAlgorithm {
  BulkOnly,
  DH,
  DHE,
  RSA,
  ECDH,
  ECDHE
}

/* We don't support arbitrary curves.  It's a terrible
 * idea and unnecessary attack surface.  Please,
 * get a grip. */
#[derive(Debug)]
pub struct ECParameters {
  pub curve_type: ECCurveType,
  pub named_group: NamedGroup
}

impl Codec for ECParameters {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.curve_type.encode(bytes);
    self.named_group.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<ECParameters> {
    let ct = try_ret!(ECCurveType::read(r));

    if ct != ECCurveType::NamedCurve {
      return None;
    }

    let grp = try_ret!(NamedGroup::read(r));

    Some(ECParameters { curve_type: ct, named_group: grp })
  }
}

#[derive(Debug, Clone)]
pub struct DigitallySignedStruct {
  pub scheme: SignatureScheme,
  pub sig: PayloadU16
}

impl DigitallySignedStruct {
  pub fn new(scheme: SignatureScheme, sig: Vec<u8>) -> DigitallySignedStruct {
    DigitallySignedStruct { scheme: scheme, sig: PayloadU16::new(sig) }
  }
}

impl Codec for DigitallySignedStruct {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.scheme.encode(bytes);
    self.sig.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<DigitallySignedStruct> {
    let scheme = try_ret!(SignatureScheme::read(r));
    let sig = try_ret!(PayloadU16::read(r));

    Some(DigitallySignedStruct { scheme: scheme, sig: sig })
  }
}

#[derive(Debug)]
pub struct ClientECDHParams {
  pub public: PayloadU8
}

impl Codec for ClientECDHParams {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.public.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<ClientECDHParams> {
    let pb = try_ret!(PayloadU8::read(r));
    Some(ClientECDHParams { public: pb })
  }
}

#[derive(Debug)]
pub struct ServerECDHParams {
  pub curve_params: ECParameters,
  pub public: PayloadU8
}

impl ServerECDHParams {
  pub fn new(named_group: &NamedGroup, pubkey: &Vec<u8>) -> ServerECDHParams {
    ServerECDHParams {
      curve_params: ECParameters {
        curve_type: ECCurveType::NamedCurve,
        named_group: *named_group
      },
      public: PayloadU8::new(pubkey.clone())
    }
  }
}

impl Codec for ServerECDHParams {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.curve_params.encode(bytes);
    self.public.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<ServerECDHParams> {
    let cp = try_ret!(ECParameters::read(r));
    let pb = try_ret!(PayloadU8::read(r));

    Some(ServerECDHParams { curve_params: cp, public: pb })
  }
}

#[derive(Debug)]
pub struct ECDHEServerKeyExchange {
  pub params: ServerECDHParams,
  pub dss: DigitallySignedStruct
}

impl Codec for ECDHEServerKeyExchange {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.params.encode(bytes);
    self.dss.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<ECDHEServerKeyExchange> {
    let params = try_ret!(ServerECDHParams::read(r));
    let dss = try_ret!(DigitallySignedStruct::read(r));

    Some(ECDHEServerKeyExchange { params: params, dss: dss })
  }
}

#[derive(Debug)]
pub enum ServerKeyExchangePayload {
  ECDHE(ECDHEServerKeyExchange),
  Unknown(Payload)
}

impl Codec for ServerKeyExchangePayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    match *self {
      ServerKeyExchangePayload::ECDHE(ref x) => x.encode(bytes),
      ServerKeyExchangePayload::Unknown(ref x) => x.encode(bytes)
    }
  }

  fn read(r: &mut Reader) -> Option<ServerKeyExchangePayload> {
    /* read as Unknown, fully parse when we know the
     * KeyExchangeAlgorithm */
    Payload::read(r).and_then(|x| Some(ServerKeyExchangePayload::Unknown(x)))
  }
}

impl ServerKeyExchangePayload {
  pub fn unwrap_given_kxa(&self, kxa: &KeyExchangeAlgorithm) -> Option<ServerKeyExchangePayload> {
    if let ServerKeyExchangePayload::Unknown(ref unk) = *self {
      let mut rd = Reader::init(&unk.0);

      return match *kxa {
        KeyExchangeAlgorithm::ECDHE =>
          ECDHEServerKeyExchange::read(&mut rd).and_then(|x| Some(ServerKeyExchangePayload::ECDHE(x))),
        _ => None
      };
    }

    None
  }

  pub fn encode_params(&self, bytes: &mut Vec<u8>) {
    bytes.clear();

    match *self {
      ServerKeyExchangePayload::ECDHE(ref x) => x.params.encode(bytes),
      _ => (),
    };
  }

  pub fn get_sig(&self) -> Option<DigitallySignedStruct> {
    match *self {
      ServerKeyExchangePayload::ECDHE(ref x) => Some(x.dss.clone()),
      _ => None
    }
  }
}

/* -- EncryptedExtensions (TLS1.3 only) -- */
declare_u16_vec!(EncryptedExtensions, ServerExtension);

/* -- CertificateRequest and sundries -- */
declare_u8_vec!(ClientCertificateTypes, ClientCertificateType);
pub type DistinguishedName = PayloadU16;
declare_u16_vec!(DistinguishedNames, DistinguishedName);

#[derive(Debug)]
pub struct CertificateRequestPayload {
  pub certtypes: ClientCertificateTypes,
  pub sigschemes: SupportedSignatureSchemes,
  pub canames: DistinguishedNames
}

impl Codec for CertificateRequestPayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.certtypes.encode(bytes);
    self.sigschemes.encode(bytes);
    self.canames.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<CertificateRequestPayload> {
    let certtypes = try_ret!(ClientCertificateTypes::read(r));
    let sigschemes = try_ret!(SupportedSignatureSchemes::read(r));
    let canames = try_ret!(DistinguishedNames::read(r));

    Some(CertificateRequestPayload {
      certtypes: certtypes,
      sigschemes: sigschemes,
      canames: canames
    })
  }
}

/* -- NewSessionTicket -- */
#[derive(Debug)]
pub struct NewSessionTicketPayload {
  pub lifetime_hint: u32,
  pub ticket: PayloadU16
}

impl NewSessionTicketPayload {
  pub fn new(lifetime_hint: u32, ticket: Vec<u8>) -> NewSessionTicketPayload {
    NewSessionTicketPayload {
      lifetime_hint: lifetime_hint,
      ticket: PayloadU16::new(ticket)
    }
  }
}

impl Codec for NewSessionTicketPayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    codec::encode_u32(self.lifetime_hint, bytes);
    self.ticket.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<NewSessionTicketPayload> {
    let lifetime = try_ret!(codec::read_u32(r));
    let ticket = try_ret!(PayloadU16::read(r));

    Some(NewSessionTicketPayload {
      lifetime_hint: lifetime,
      ticket: ticket
    })
  }
}

/* -- NewSessionTicket electric boogaloo -- */
#[derive(Debug)]
pub enum NewSessionTicketExtension {
  Unknown(UnknownExtension)
}

impl NewSessionTicketExtension {
  pub fn get_type(&self) -> ExtensionType {
    match *self {
      NewSessionTicketExtension::Unknown(ref r) => r.typ
    }
  }
}

impl Codec for NewSessionTicketExtension {
  fn encode(&self, bytes: &mut Vec<u8>) {
    self.get_type().encode(bytes);

    let mut sub: Vec<u8> = Vec::new();
    match *self {
      NewSessionTicketExtension::Unknown(ref r) => r.encode(&mut sub)
    }

    codec::encode_u16(sub.len() as u16, bytes);
    bytes.append(&mut sub);
  }

  fn read(r: &mut Reader) -> Option<NewSessionTicketExtension> {
    let typ = try_ret!(ExtensionType::read(r));
    let len = try_ret!(codec::read_u16(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    Some(match typ {
      _ =>
        NewSessionTicketExtension::Unknown(try_ret!(UnknownExtension::read(typ, &mut sub)))
    })
  }
}

declare_u16_vec!(NewSessionTicketExtensions, NewSessionTicketExtension);

#[derive(Debug)]
pub struct NewSessionTicketPayloadTLS13 {
  pub lifetime: u32,
  pub age_add: u32,
  pub ticket: PayloadU16,
  pub exts: NewSessionTicketExtensions
}

impl Codec for NewSessionTicketPayloadTLS13 {
  fn encode(&self, bytes: &mut Vec<u8>) {
    codec::encode_u32(self.lifetime, bytes);
    codec::encode_u32(self.age_add, bytes);
    self.ticket.encode(bytes);
    self.exts.encode(bytes);
  }

  fn read(r: &mut Reader) -> Option<NewSessionTicketPayloadTLS13> {
    let lifetime = try_ret!(codec::read_u32(r));
    let age_add = try_ret!(codec::read_u32(r));
    let ticket = try_ret!(PayloadU16::read(r));
    let exts = try_ret!(NewSessionTicketExtensions::read(r));

    Some(NewSessionTicketPayloadTLS13 {
      lifetime: lifetime,
      age_add: age_add,
      ticket: ticket,
      exts: exts
    })
  }
}

#[derive(Debug)]
pub enum HandshakePayload {
  HelloRequest,
  ClientHello(ClientHelloPayload),
  ServerHello(ServerHelloPayload),
  HelloRetryRequest(HelloRetryRequest),
  Certificate(CertificatePayload),
  CertificateTLS13(CertificatePayloadTLS13),
  ServerKeyExchange(ServerKeyExchangePayload),
  CertificateRequest(CertificateRequestPayload),
  CertificateVerify(DigitallySignedStruct),
  ServerHelloDone,
  ClientKeyExchange(Payload),
  NewSessionTicket(NewSessionTicketPayload),
  NewSessionTicketTLS13(NewSessionTicketPayloadTLS13),
  EncryptedExtensions(EncryptedExtensions),
  KeyUpdate(KeyUpdateRequest),
  Finished(Payload),
  Unknown(Payload)
}

impl HandshakePayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    match *self {
      HandshakePayload::HelloRequest => {},
      HandshakePayload::ClientHello(ref x) => x.encode(bytes),
      HandshakePayload::ServerHello(ref x) => x.encode(bytes),
      HandshakePayload::HelloRetryRequest(ref x) => x.encode(bytes),
      HandshakePayload::Certificate(ref x) => x.encode(bytes),
      HandshakePayload::CertificateTLS13(ref x) => x.encode(bytes),
      HandshakePayload::ServerKeyExchange(ref x) => x.encode(bytes),
      HandshakePayload::ServerHelloDone => {},
      HandshakePayload::ClientKeyExchange(ref x) => x.encode(bytes),
      HandshakePayload::CertificateRequest(ref x) => x.encode(bytes),
      HandshakePayload::CertificateVerify(ref x) => x.encode(bytes),
      HandshakePayload::NewSessionTicket(ref x) => x.encode(bytes),
      HandshakePayload::NewSessionTicketTLS13(ref x) => x.encode(bytes),
      HandshakePayload::EncryptedExtensions(ref x) => x.encode(bytes),
      HandshakePayload::KeyUpdate(ref x) => x.encode(bytes),
      HandshakePayload::Finished(ref x) => x.encode(bytes),
      HandshakePayload::Unknown(ref x) => x.encode(bytes)
    }
  }
}

#[derive(Debug)]
pub struct HandshakeMessagePayload {
  pub typ: HandshakeType,
  pub payload: HandshakePayload
}

impl Codec for HandshakeMessagePayload {
  fn encode(&self, bytes: &mut Vec<u8>) {
    /* encode payload to learn length */
    let mut sub: Vec<u8> = Vec::new();
    self.payload.encode(&mut sub);

    /* output type, length, and encoded payload */
    self.typ.encode(bytes);
    codec::encode_u24(sub.len() as u32, bytes);
    bytes.append(&mut sub);
  }

  fn read(r: &mut Reader) -> Option<HandshakeMessagePayload> {
    HandshakeMessagePayload::read_version(r, ProtocolVersion::TLSv1_2)
  }
}

impl HandshakeMessagePayload {
  pub fn len(&self) -> usize {
    let mut buf = Vec::new();
    self.encode(&mut buf);
    buf.len()
  }

  pub fn read_version(r: &mut Reader, vers: ProtocolVersion) -> Option<HandshakeMessagePayload> {
    let typ = try_ret!(HandshakeType::read(r));
    let len = try_ret!(codec::read_u24(r)) as usize;
    let mut sub = try_ret!(r.sub(len));

    let payload = match typ {
      HandshakeType::HelloRequest if sub.left() == 0 =>
        HandshakePayload::HelloRequest,
      HandshakeType::ClientHello =>
        HandshakePayload::ClientHello(try_ret!(ClientHelloPayload::read(&mut sub))),
      HandshakeType::ServerHello =>
        HandshakePayload::ServerHello(try_ret!(ServerHelloPayload::read(&mut sub))),
      HandshakeType::HelloRetryRequest =>
        HandshakePayload::HelloRetryRequest(try_ret!(HelloRetryRequest::read(&mut sub))),
      HandshakeType::Certificate if vers == ProtocolVersion::TLSv1_3 =>
        HandshakePayload::CertificateTLS13(try_ret!(CertificatePayloadTLS13::read(&mut sub))),
      HandshakeType::Certificate =>
        HandshakePayload::Certificate(try_ret!(CertificatePayload::read(&mut sub))),
      HandshakeType::ServerKeyExchange =>
        HandshakePayload::ServerKeyExchange(try_ret!(ServerKeyExchangePayload::read(&mut sub))),
      HandshakeType::ServerHelloDone if sub.left() == 0 =>
        HandshakePayload::ServerHelloDone,
      HandshakeType::ClientKeyExchange =>
        HandshakePayload::ClientKeyExchange(try_ret!(Payload::read(&mut sub))),
      HandshakeType::CertificateRequest =>
        HandshakePayload::CertificateRequest(try_ret!(CertificateRequestPayload::read(&mut sub))),
      HandshakeType::CertificateVerify =>
        HandshakePayload::CertificateVerify(try_ret!(DigitallySignedStruct::read(&mut sub))),
      HandshakeType::NewSessionTicket if vers == ProtocolVersion::TLSv1_3  =>
        HandshakePayload::NewSessionTicketTLS13(try_ret!(NewSessionTicketPayloadTLS13::read(&mut sub))),
      HandshakeType::NewSessionTicket =>
        HandshakePayload::NewSessionTicket(try_ret!(NewSessionTicketPayload::read(&mut sub))),
      HandshakeType::EncryptedExtensions =>
        HandshakePayload::EncryptedExtensions(try_ret!(EncryptedExtensions::read(&mut sub))),
      HandshakeType::KeyUpdate =>
        HandshakePayload::KeyUpdate(try_ret!(KeyUpdateRequest::read(&mut sub))),
      HandshakeType::Finished =>
        HandshakePayload::Finished(try_ret!(Payload::read(&mut sub))),
      _ =>
        HandshakePayload::Unknown(try_ret!(Payload::read(&mut sub)))
    };

    Some(HandshakeMessagePayload { typ: typ, payload: payload })
  }
}
