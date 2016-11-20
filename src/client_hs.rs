use msgs::enums::{ContentType, HandshakeType, ExtensionType};
use msgs::enums::{Compression, ProtocolVersion, AlertDescription, NamedGroup};
use msgs::message::{Message, MessagePayload};
use msgs::base::{Payload, PayloadU8};
use msgs::handshake::{HandshakePayload, HandshakeMessagePayload, ClientHelloPayload};
use msgs::handshake::{SessionID, Random, ServerHelloPayload};
use msgs::handshake::{ClientExtension, ServerExtension};
use msgs::handshake::{SupportedSignatureSchemes, SupportedMandatedSignatureSchemes};
use msgs::handshake::DecomposedSignatureScheme;
use msgs::handshake::{NamedGroups, SupportedGroups, KeyShareEntry};
use msgs::handshake::{ECPointFormatList, SupportedPointFormats};
use msgs::handshake::{ProtocolNameList, ConvertProtocolNameList};
use msgs::handshake::ServerKeyExchangePayload;
use msgs::handshake::DigitallySignedStruct;
use msgs::enums::ClientCertificateType;
use msgs::codec::Codec;
use msgs::persist;
use msgs::ccs::ChangeCipherSpecPayload;
use client::{ClientSessionImpl, ConnState};
use session::SessionSecrets;
use key_schedule::{KeySchedule, SecretKind};
use cipher::MessageCipher;
use suites;
use verify;
use rand;
use error::TLSError;
use handshake::Expectation;

use std::mem;

// draft-ietf-tls-tls13-18
const TLS13_DRAFT: u16 = 0x7f12;

macro_rules! extract_handshake(
  ( $m:expr, $t:path ) => (
    match $m.payload {
      MessagePayload::Handshake(ref hsp) => match hsp.payload {
        $t(ref hm) => Some(hm),
        _ => None
      },
      _ => None
    }
  )
);

pub type HandleFunction = fn(&mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError>;

/* These are effectively operations on the ClientSessionImpl, variant on the
 * connection state. They must not have state of their own -- so they're
 * functions rather than a trait. */
pub struct Handler {
  pub expect: Expectation,
  pub handle: HandleFunction
}

fn find_session(sess: &mut ClientSessionImpl) -> Option<persist::ClientSessionValue> {
  let key = persist::ClientSessionKey::for_dns_name(&sess.handshake_data.dns_name);
  let key_buf = key.get_encoding();

  let mut persist = sess.config.session_persistence.lock().unwrap();
  let maybe_value = persist.get(&key_buf);

  if maybe_value.is_none() {
    info!("No cached session for {:?}", sess.handshake_data.dns_name);
    return None
  }

  let value = maybe_value.unwrap();
  persist::ClientSessionValue::read_bytes(&value)
}

/// If we have a ticket, we use the sessionid as a signal that we're
/// doing an abbreviated handshake.  See section 3.4 in RFC5077.
fn randomise_sessionid_for_ticket(csv: &mut persist::ClientSessionValue) {
  if csv.ticket.len() > 0 {
    let mut random_id = [0u8; 16];
    rand::fill_random(&mut random_id);
    csv.session_id = SessionID::new(random_id.to_vec());
  }
}

pub fn emit_client_hello(sess: &mut ClientSessionImpl) {
  /* Do we have a SessionID or ticket cached for this host? */
  sess.handshake_data.resuming_session = find_session(sess);
  let (session_id, ticket) = if sess.handshake_data.resuming_session.is_some() {
    let mut resuming = sess.handshake_data.resuming_session.as_mut().unwrap();
    randomise_sessionid_for_ticket(resuming);
    info!("Resuming session");
    (resuming.session_id.clone(), resuming.ticket.0.clone())
  } else {
    info!("Not resuming any session");
    (SessionID::empty(), Vec::new())
  };

  let supported_versions = vec![
    ProtocolVersion::Unknown(TLS13_DRAFT),
    ProtocolVersion::TLSv1_2
  ];

  let mut key_shares = vec![];
  let groups = NamedGroups::supported();

  for group in groups {
    if let Some(key_share) = suites::KeyExchange::start_ecdhe(group) {
      key_shares.push(KeyShareEntry::new(group, &key_share.pubkey));
      sess.handshake_data.offered_key_shares.push(key_share);
    }
  }

  let mut exts = Vec::new();
  exts.push(ClientExtension::SupportedVersions(supported_versions));
  exts.push(ClientExtension::make_sni(&sess.handshake_data.dns_name));
  exts.push(ClientExtension::ECPointFormats(ECPointFormatList::supported()));
  exts.push(ClientExtension::NamedGroups(NamedGroups::supported()));
  exts.push(ClientExtension::SignatureAlgorithms(SupportedSignatureSchemes::supported_verify()));
  exts.push(ClientExtension::KeyShare(key_shares));

  if sess.config.enable_tickets {
    /* If we have a ticket, include it.  Otherwise, request one. */
    if ticket.is_empty() {
      exts.push(ClientExtension::SessionTicketRequest);
    } else {
      exts.push(ClientExtension::SessionTicketOffer(Payload::new(ticket)));
    }
  }

  if !sess.config.alpn_protocols.is_empty() {
    exts.push(ClientExtension::Protocols(ProtocolNameList::from_strings(&sess.config.alpn_protocols)));
  }

  /* Note what extensions we sent. */
  sess.handshake_data.sent_extensions = exts.iter()
    .map(|ext| ext.get_type())
    .collect();

  let ch = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ClientHello,
        payload: HandshakePayload::ClientHello(
          ClientHelloPayload {
            client_version: ProtocolVersion::TLSv1_2,
            random: Random::from_slice(&sess.handshake_data.randoms.client),
            session_id: session_id,
            cipher_suites: sess.get_cipher_suites(),
            compression_methods: vec![Compression::Null],
            extensions: exts
          }
        )
      }
    )
  };

  debug!("Sending ClientHello {:#?}", ch);

  sess.handshake_data.transcript.add_message(&ch);
  sess.common.send_msg(ch, false);
}

fn sent_unsolicited_extensions(sess: &ClientSessionImpl, exts: &Vec<ServerExtension>) -> bool {
  let allowed_unsolicited = vec![ ExtensionType::RenegotiationInfo ];

  let sent = &sess.handshake_data.sent_extensions;
  for ext in exts {
    let ext_type = ext.get_type();
    if !sent.contains(&ext_type) && !allowed_unsolicited.contains(&ext_type) {
      debug!("Unsolicited extension {:?}", ext_type);
      return true;
    }
  }

  false
}

fn find_key_share(sess: &mut ClientSessionImpl, group: NamedGroup) -> Result<suites::KeyExchange, TLSError> {
  /* While we're doing this, discard all the other key shares. */
  while !sess.handshake_data.offered_key_shares.is_empty() {
    let share = sess.handshake_data.offered_key_shares.remove(0);
    if share.group == group {
      sess.handshake_data.offered_key_shares.clear();
      return Ok(share);
    }
  }

  sess.common.send_fatal_alert(AlertDescription::IllegalParameter);
  Err(TLSError::PeerMisbehavedError("wrong group for key share".to_string()))
}

fn start_handshake_traffic(sess: &mut ClientSessionImpl, server_hello: &ServerHelloPayload)
  -> Result<(), TLSError> {
  let their_key_share = try!(
    server_hello.get_key_share()
      .ok_or_else(|| {
        sess.common.send_fatal_alert(AlertDescription::MissingExtension);
        TLSError::PeerMisbehavedError("missing key share".to_string())
      })
  );

  let our_key_share = try!(find_key_share(sess, their_key_share.group));
  let shared = try!(
    our_key_share.complete(&their_key_share.payload.0)
      .ok_or_else(|| TLSError::PeerMisbehavedError("key exchange failed".to_string()))
  );

  let suite = sess.handshake_data.ciphersuite.as_ref().unwrap();
  let hash = suite.get_hash();
  let mut key_schedule = KeySchedule::new(hash);
  key_schedule.input_empty(); /* TODO: insert PSK here */
  key_schedule.input_secret(&shared.premaster_secret);

  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let write_key = key_schedule.derive(SecretKind::ClientHandshakeTrafficSecret, &handshake_hash);
  let read_key = key_schedule.derive(SecretKind::ServerHandshakeTrafficSecret, &handshake_hash);
  sess.common.set_message_cipher(MessageCipher::new_tls13(suite, &write_key, &read_key));
  key_schedule.current_client_traffic_secret = write_key;
  key_schedule.current_server_traffic_secret = read_key;
  sess.key_schedule = Some(key_schedule);

  Ok(())
}

fn handle_server_hello(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let server_hello = extract_handshake!(m, HandshakePayload::ServerHello).unwrap();
  debug!("We got ServerHello {:#?}", server_hello);

  match server_hello.server_version {
    ProtocolVersion::TLSv1_2 => {
      sess.common.is_tls13 = false;
    },
    ProtocolVersion::TLSv1_3 | ProtocolVersion::Unknown(TLS13_DRAFT) => {
      sess.common.is_tls13 = true;
    },
    _ => {
      sess.common.send_fatal_alert(AlertDescription::HandshakeFailure);
      return Err(TLSError::PeerIncompatibleError("server does not support TLS v1.2/v1.3".to_string()));
    }
  };

  if server_hello.compression_method != Compression::Null {
    sess.common.send_fatal_alert(AlertDescription::HandshakeFailure);
    return Err(TLSError::PeerMisbehavedError("server chose non-Null compression".to_string()));
  }

  if server_hello.has_duplicate_extension() {
    sess.common.send_fatal_alert(AlertDescription::DecodeError);
    return Err(TLSError::PeerMisbehavedError("server sent duplicate extensions".to_string()));
  }

  if sent_unsolicited_extensions(sess, &server_hello.extensions) {
    sess.common.send_fatal_alert(AlertDescription::UnsupportedExtension);
    return Err(TLSError::PeerMisbehavedError("server sent unsolicited extension".to_string()));
  }

  /* Extract ALPN protocol */
  sess.alpn_protocol = server_hello.get_alpn_protocol();
  if sess.alpn_protocol.is_some() {
    if !sess.config.alpn_protocols.contains(sess.alpn_protocol.as_ref().unwrap()) {
      sess.common.send_fatal_alert(AlertDescription::IllegalParameter);
      return Err(TLSError::PeerMisbehavedError("server sent non-offered ALPN protocol".to_string()));
    }
  }
  info!("ALPN protocol is {:?}", sess.alpn_protocol);

  let scs = sess.find_cipher_suite(&server_hello.cipher_suite);

  if scs.is_none() {
    sess.common.send_fatal_alert(AlertDescription::HandshakeFailure);
    return Err(TLSError::PeerMisbehavedError("server chose non-offered ciphersuite".to_string()));
  }

  info!("Using ciphersuite {:?}", server_hello.cipher_suite);

  /* Start our handshake hash, and input the client-hello. */
  sess.handshake_data.transcript.start_hash(scs.as_ref().unwrap().get_hash());
  sess.handshake_data.transcript.add_message(&m);

  sess.handshake_data.ciphersuite = scs;

  /* For TLS1.3, start message encryption using
   * handshake_traffic_secret. */
  if sess.common.is_tls13 {
    try!(start_handshake_traffic(sess, &server_hello));
    return Ok(ConnState::ExpectEncryptedExtensions);
  }

  /* TLS1.2 only from here-on */

  /* Save ServerRandom and SessionID */
  server_hello.random.write_slice(&mut sess.handshake_data.randoms.server);
  sess.handshake_data.session_id = server_hello.session_id.clone();

  /* Might the server send a ticket? */
  if server_hello.find_extension(ExtensionType::SessionTicket).is_some() {
    info!("Server supports tickets");
    sess.handshake_data.must_issue_new_ticket = true;
  }

  /* See if we're successfully resuming. */
  let mut abbreviated_handshake = false;
  if let Some(ref resuming) = sess.handshake_data.resuming_session {
    if resuming.session_id == sess.handshake_data.session_id {
      info!("Server agreed to resume");
      abbreviated_handshake = true;

      /* Is the server telling lies about the ciphersuite? */
      if resuming.cipher_suite != scs.unwrap().suite {
        let error_msg = "abbreviated handshake offered, but with varied cs".to_string();
        return Err(TLSError::PeerMisbehavedError(error_msg));
      }

      sess.secrets = Some(SessionSecrets::new_resume(&sess.handshake_data.randoms,
                                                     scs.unwrap().get_hash(),
                                                     &resuming.master_secret.0));
    }
  }

  if abbreviated_handshake {
    sess.start_encryption_tls12();

    if sess.handshake_data.must_issue_new_ticket {
      Ok(ConnState::ExpectNewTicketResume)
    } else {
      Ok(ConnState::ExpectCCSResume)
    }
  } else {
    Ok(ConnState::ExpectCertificate)
  }
}

pub static EXPECT_SERVER_HELLO: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::ServerHello]
  },
  handle: handle_server_hello
};

fn handle_encrypted_extensions(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let _exts = extract_handshake!(m, HandshakePayload::EncryptedExtensions).unwrap();
  info!("TLS1.3 encrypted extensions: {:?}", _exts);
  sess.handshake_data.transcript.add_message(&m);
  Ok(ConnState::ExpectCertificate)
}

pub static EXPECT_ENCRYPTED_EXTENSIONS: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::EncryptedExtensions]
  },
  handle: handle_encrypted_extensions
};

fn handle_certificate(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  sess.handshake_data.transcript.add_message(&m);

  if sess.common.is_tls13 {
    let cert_chain = extract_handshake!(m, HandshakePayload::CertificateTLS13).unwrap();
    sess.handshake_data.server_cert_chain = cert_chain.convert();
    Ok(ConnState::ExpectCertificateVerify)
  } else {
    let cert_chain = extract_handshake!(m, HandshakePayload::Certificate).unwrap();
    sess.handshake_data.server_cert_chain = cert_chain.clone();
    Ok(ConnState::ExpectServerKX)
  }
}

pub static EXPECT_CERTIFICATE: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::Certificate]
  },
  handle: handle_certificate
};

fn handle_server_kx(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let opaque_kx = extract_handshake!(m, HandshakePayload::ServerKeyExchange).unwrap();
  let maybe_decoded_kx = opaque_kx.unwrap_given_kxa(&sess.handshake_data.ciphersuite.unwrap().kx);
  sess.handshake_data.transcript.add_message(&m);

  if maybe_decoded_kx.is_none() {
    return Err(TLSError::PeerIncompatibleError("cannot decode server's kx".to_string()));
  }

  let decoded_kx = maybe_decoded_kx.unwrap();

  /* Save the signature and signed parameters for later verification. */
  sess.handshake_data.server_kx_sig = decoded_kx.get_sig();
  decoded_kx.encode_params(&mut sess.handshake_data.server_kx_params);

  match decoded_kx {
    ServerKeyExchangePayload::ECDHE(ecdhe) => info!("ECDHE curve is {:?}", ecdhe.params.curve_params),
    _ => ()
  }

  Ok(ConnState::ExpectServerHelloDoneOrCertRequest)
}

pub static EXPECT_SERVER_KX: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::ServerKeyExchange]
  },
  handle: handle_server_kx
};

/* --- TLS1.3 CertificateVerify --- */
fn handle_certificate_verify(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let cert_verify = extract_handshake!(m, HandshakePayload::CertificateVerify).unwrap();

  /* 1. Verify the certificate chain.
   * 2. Verify their signature on the handshake. */
  try!(verify::verify_server_cert(&sess.config.root_store,
                                  &sess.handshake_data.server_cert_chain,
                                  &sess.handshake_data.dns_name));

  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  try!(verify::verify_tls13(&sess.handshake_data.server_cert_chain[0],
                            &cert_verify,
                            &handshake_hash,
                            b"TLS 1.3, server CertificateVerify\x00"));

  sess.handshake_data.transcript.add_message(&m);

  Ok(ConnState::ExpectFinished)
}

pub static EXPECT_CERTIFICATE_VERIFY: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::CertificateVerify]
  },
  handle: handle_certificate_verify
};

fn emit_certificate(sess: &mut ClientSessionImpl) {
  let chosen_cert = sess.handshake_data.client_auth_cert.take();

  let cert = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::Certificate,
        payload: HandshakePayload::Certificate(
          chosen_cert.unwrap_or_else(Vec::new)
        )
      }
    )
  };

  sess.handshake_data.transcript.add_message(&cert);
  sess.common.send_msg(cert, false);
}

fn emit_clientkx(sess: &mut ClientSessionImpl, kxd: &suites::KeyExchangeResult) {
  let mut buf = Vec::new();
  let ecpoint = PayloadU8::new(kxd.pubkey.clone());
  ecpoint.encode(&mut buf);
  let pubkey = Payload::new(buf);

  let ckx = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ClientKeyExchange,
        payload: HandshakePayload::ClientKeyExchange(pubkey)
      }
    )
  };

  sess.handshake_data.transcript.add_message(&ckx);
  sess.common.send_msg(ckx, false);
}

fn emit_certverify(sess: &mut ClientSessionImpl) {
  if sess.handshake_data.client_auth_key.is_none() {
    debug!("Not sending CertificateVerify, no key");
    sess.handshake_data.transcript.abandon_client_auth();
    return;
  }

  let message = sess.handshake_data.transcript.take_handshake_buf();
  let key = sess.handshake_data.client_auth_key.take().unwrap();
  let sigscheme = sess.handshake_data.client_auth_sigscheme
    .clone()
    .unwrap();
  let sig = key.sign(sigscheme, &message)
    .expect("client auth signing failed unexpectedly");
  let body = DigitallySignedStruct::new(sigscheme, sig);

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::CertificateVerify,
        payload: HandshakePayload::CertificateVerify(body)
      }
    )
  };

  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, false);
}

fn emit_ccs(sess: &mut ClientSessionImpl) {
  let ccs = Message {
    typ: ContentType::ChangeCipherSpec,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::ChangeCipherSpec(ChangeCipherSpecPayload {})
  };

  sess.common.send_msg(ccs, false);
  sess.common.we_now_encrypting();
}

fn emit_finished(sess: &mut ClientSessionImpl) {
  let vh = sess.handshake_data.transcript.get_current_hash();
  let verify_data = sess.secrets.as_ref().unwrap().client_verify_data(&vh);
  let verify_data_payload = Payload::new(verify_data);

  let f = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::Finished,
        payload: HandshakePayload::Finished(verify_data_payload)
      }
    )
  };

  sess.handshake_data.transcript.add_message(&f);
  sess.common.send_msg(f, true);
}

/* --- Either a CertificateRequest, or a ServerHelloDone. ---
 * Existence of the CertificateRequest tells us the server is asking for
 * client auth.  Otherwise we go straight to ServerHelloDone. */
fn handle_certificate_req(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let certreq = extract_handshake!(m, HandshakePayload::CertificateRequest).unwrap();
  sess.handshake_data.transcript.add_message(&m);
  sess.handshake_data.doing_client_auth = true;
  info!("Got CertificateRequest {:?}", certreq);

  /* The RFC jovially describes the design here as 'somewhat complicated'
   * and 'somewhat underspecified'.  So thanks for that. */

  /* We only support RSA signing at the moment.  If you don't support that,
   * we're not doing client auth. */
  if !certreq.certtypes.contains(&ClientCertificateType::RSASign) {
    warn!("Server asked for client auth but without RSASign");
    return Ok(ConnState::ExpectServerHelloDone);
  }

  let maybe_certkey = sess.config.client_auth_cert_resolver.resolve(
    &certreq.canames, &certreq.sigschemes
  );

  let scs = sess.handshake_data.ciphersuite.as_ref().unwrap();
  let maybe_sigscheme = scs.resolve_sig_scheme(&certreq.sigschemes);

  if maybe_certkey.is_some() && maybe_sigscheme.is_some() {
    let (cert, key) = maybe_certkey.unwrap();
    info!("Attempting client auth, will use {:?}", maybe_sigscheme.as_ref().unwrap());
    sess.handshake_data.client_auth_cert = Some(cert);
    sess.handshake_data.client_auth_key = Some(key);
    sess.handshake_data.client_auth_sigscheme = maybe_sigscheme;
  } else {
    info!("Client auth requested but no cert/sigscheme available");
  }

  Ok(ConnState::ExpectServerHelloDone)
}

fn handle_done_or_certreq(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  if extract_handshake!(m, HandshakePayload::CertificateRequest).is_some() {
    handle_certificate_req(sess, m)
  } else {
    sess.handshake_data.transcript.abandon_client_auth();
    handle_server_hello_done(sess, m)
  }
}

pub static EXPECT_DONE_OR_CERTREQ: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::CertificateRequest, HandshakeType::ServerHelloDone]
  },
  handle: handle_done_or_certreq
};

fn handle_server_hello_done(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  sess.handshake_data.transcript.add_message(&m);

  info!("Server cert is {:?}", sess.handshake_data.server_cert_chain);
  info!("Server DNS name is {:?}", sess.handshake_data.dns_name);

  /* 1. Verify the cert chain.
   * 2. Verify that the top certificate signed their kx.
   * 3. If doing client auth, send our Certificate.
   * 4. Complete the key exchange:
   *    a) generate our kx pair
   *    b) emit a ClientKeyExchange containing it
   *    c) if doing client auth, emit a CertificateVerify
   *    d) emit a CCS
   *    e) derive the shared keys, and start encryption
   * 5. emit a Finished, our first encrypted message under the new keys. */

  /* 1. */
  try!(verify::verify_server_cert(&sess.config.root_store,
                                  &sess.handshake_data.server_cert_chain,
                                  &sess.handshake_data.dns_name));

  /* 2. */
  /* Build up the contents of the signed message.
   * It's ClientHello.random || ServerHello.random || ServerKeyExchange.params */
  {
    let mut message = Vec::new();
    message.extend_from_slice(&sess.handshake_data.randoms.client);
    message.extend_from_slice(&sess.handshake_data.randoms.server);
    message.extend_from_slice(&sess.handshake_data.server_kx_params);

    /* Check the signature is compatible with the ciphersuite. */
    let sig = sess.handshake_data.server_kx_sig.as_ref().unwrap();
    let scs = sess.handshake_data.ciphersuite.as_ref().unwrap();
    if scs.sign != sig.scheme.sign() {
      let error_message = format!("peer signed kx with wrong algorithm (got {:?} expect {:?})",
                                  sig.scheme.sign(), scs.sign);
      return Err(TLSError::PeerMisbehavedError(error_message));
    }

    try!(verify::verify_signed_struct(&message,
                                      &sess.handshake_data.server_cert_chain[0],
                                      sig));
  }

  /* 3. */
  if sess.handshake_data.doing_client_auth {
    emit_certificate(sess);
  }

  /* 4a. */
  let kxd = try!(sess.handshake_data.ciphersuite.as_ref().unwrap()
    .do_client_kx(&sess.handshake_data.server_kx_params)
    .ok_or_else(|| TLSError::PeerMisbehavedError("key exchange failed".to_string()))
  );

  /* 4b. */
  emit_clientkx(sess, &kxd);

  /* 4c. */
  if sess.handshake_data.doing_client_auth {
    emit_certverify(sess);
  }

  /* 4d. */
  emit_ccs(sess);

  /* 4e. Now commit secrets. */
  let hashalg = sess.handshake_data.ciphersuite.as_ref().unwrap().get_hash();
  sess.secrets = Some(SessionSecrets::new(&sess.handshake_data.randoms,
                                          hashalg,
                                          &kxd.premaster_secret));
  sess.start_encryption_tls12();

  /* 5. */
  emit_finished(sess);

  if sess.handshake_data.must_issue_new_ticket {
    Ok(ConnState::ExpectNewTicket)
  } else {
    Ok(ConnState::ExpectCCS)
  }
}

pub static EXPECT_SERVER_HELLO_DONE: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::ServerHelloDone]
  },
  handle: handle_server_hello_done
};

/* -- Waiting for their CCS -- */
fn handle_ccs(sess: &mut ClientSessionImpl, _m: Message) -> Result<ConnState, TLSError> {
  /* CCS should not be received interleaved with fragmented handshake-level
   * message. */
  if !sess.common.handshake_joiner.empty() {
    warn!("CCS received interleaved with fragmented handshake");
    return Err(TLSError::InappropriateMessage {
      expect_types: vec![ ContentType::Handshake ],
      got_type: ContentType::ChangeCipherSpec
    });
  }

  /* nb. msgs layer validates trivial contents of CCS */
  sess.common.peer_now_encrypting();
  Ok(ConnState::ExpectFinished)
}

pub static EXPECT_CCS: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::ChangeCipherSpec],
    handshake_types: &[]
  },
  handle: handle_ccs
};

fn handle_new_ticket(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let ticket = extract_handshake!(m, HandshakePayload::NewSessionTicket).unwrap();
  sess.handshake_data.transcript.add_message(&m);
  sess.handshake_data.new_ticket = ticket.ticket.0.clone();
  sess.handshake_data.new_ticket_lifetime = ticket.lifetime_hint;
  Ok(ConnState::ExpectCCS)
}

pub static EXPECT_NEW_TICKET: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::NewSessionTicket]
  },
  handle: handle_new_ticket
};

fn handle_ccs_resume(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  handle_ccs(sess, m)
    .and(Ok(ConnState::ExpectFinishedResume))
}

pub static EXPECT_CCS_RESUME: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::ChangeCipherSpec],
    handshake_types: &[]
  },
  handle: handle_ccs_resume
};

fn handle_new_ticket_resume(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  handle_new_ticket(sess, m)
    .and(Ok(ConnState::ExpectCCSResume))
}

pub static EXPECT_NEW_TICKET_RESUME: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::NewSessionTicket]
  },
  handle: handle_new_ticket_resume
};

/* -- Waiting for their finished -- */
fn save_session(sess: &mut ClientSessionImpl) {
  /* Save a ticket.  If we got a new ticket, save that.  Otherwise, save the
   * original ticket again. */
  let mut ticket = mem::replace(&mut sess.handshake_data.new_ticket, Vec::new());
  if ticket.is_empty() && sess.handshake_data.resuming_session.is_some() {
    ticket = sess.handshake_data.resuming_session.as_mut().unwrap().take_ticket();
  }

  if sess.handshake_data.session_id.is_empty() && ticket.is_empty() {
    info!("Session not saved: server didn't allocate id or ticket");
    return;
  }

  let key = persist::ClientSessionKey::for_dns_name(&sess.handshake_data.dns_name);
  let key_buf = key.get_encoding();

  let scs = sess.handshake_data.ciphersuite.as_ref().unwrap();
  let master_secret = sess.secrets.as_ref().unwrap().get_master_secret();
  let value = persist::ClientSessionValue::new(&scs.suite,
                                               &sess.handshake_data.session_id,
                                               ticket,
                                               master_secret);
  let value_buf = value.get_encoding();

  let mut persist = sess.config.session_persistence.lock().unwrap();
  let worked = persist.put(key_buf, value_buf);

  if worked {
    info!("Session saved");
  } else {
    info!("Session not saved");
  }
}

fn handle_finished(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  if sess.common.is_tls13 {
    handle_finished_tls13(sess, m)
  } else {
    handle_finished_tls12(sess, m)
  }
}

fn emit_finished_tls13(sess: &mut ClientSessionImpl) {
  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let verify_data = sess.key_schedule
    .as_ref()
    .unwrap()
    .sign_verify_data(SecretKind::ClientHandshakeTrafficSecret, &handshake_hash);
  let verify_data_payload = Payload::new(verify_data);

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::Finished,
        payload: HandshakePayload::Finished(verify_data_payload)
      }
    )
  };

  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, true);
}

fn handle_finished_tls13(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let finished = extract_handshake!(m, HandshakePayload::Finished).unwrap();

  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let expect_verify_data = sess.key_schedule
    .as_ref()
    .unwrap()
    .sign_verify_data(SecretKind::ServerHandshakeTrafficSecret, &handshake_hash);

  use ring;
  try!(
    ring::constant_time::verify_slices_are_equal(&expect_verify_data, &finished.0)
      .map_err(|_| TLSError::DecryptError)
  );

  sess.handshake_data.transcript.add_message(&m);
  let handshake_hash = sess.handshake_data.transcript.get_current_hash();

  emit_finished_tls13(sess);

  let key_schedule = sess.key_schedule.as_mut().unwrap();
  key_schedule.input_empty();
  let write_key = key_schedule.derive(SecretKind::ClientApplicationTrafficSecret, &handshake_hash);
  let read_key = key_schedule.derive(SecretKind::ServerApplicationTrafficSecret, &handshake_hash);
  let suite = sess.handshake_data.ciphersuite.as_ref().unwrap();
  sess.common.set_message_cipher(MessageCipher::new_tls13(suite, &write_key, &read_key));
  key_schedule.current_client_traffic_secret = write_key;
  key_schedule.current_server_traffic_secret = read_key;

  Ok(ConnState::TrafficTLS13)
}

fn handle_finished_tls12(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let finished = extract_handshake!(m, HandshakePayload::Finished).unwrap();

  /* Work out what verify_data we expect. */
  let vh = sess.handshake_data.transcript.get_current_hash();
  let expect_verify_data = sess.secrets.as_ref().unwrap().server_verify_data(&vh);

  /* Constant-time verification of this is relatively unimportant: they only
   * get one chance.  But it can't hurt. */
  use ring;
  try!(
    ring::constant_time::verify_slices_are_equal(&expect_verify_data, &finished.0)
      .map_err(|_| TLSError::DecryptError)
  );

  /* Hash this message too. */
  sess.handshake_data.transcript.add_message(&m);

  save_session(sess);

  Ok(ConnState::TrafficTLS12)
}

fn handle_finished_resume(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let next_state = try!(handle_finished(sess, m));

  emit_ccs(sess);
  emit_finished(sess);
  Ok(next_state)
}

pub static EXPECT_FINISHED: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::Finished]
  },
  handle: handle_finished
};

pub static EXPECT_FINISHED_RESUME: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[]
  },
  handle: handle_finished_resume
};

/* -- Traffic transit state -- */
fn handle_traffic(sess: &mut ClientSessionImpl, mut m: Message) -> Result<ConnState, TLSError> {
  sess.common.take_received_plaintext(m.take_opaque_payload().unwrap());
  Ok(ConnState::TrafficTLS12)
}

pub static TRAFFIC_TLS12: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::ApplicationData],
    handshake_types: &[]
  },
  handle: handle_traffic
};

/* -- Traffic transit state (TLS1.3) --
 * In this state we can be sent tickets, keyupdates,
 * and application data. */
fn handle_traffic_tls13(sess: &mut ClientSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  if m.is_content_type(ContentType::ApplicationData) {
    try!(handle_traffic(sess, m));
  } else if m.is_handshake_type(HandshakeType::NewSessionTicket) {
    info!("Ignoring TLS1.3 NewSessionTicket message {:?}", m);
  }

  Ok(ConnState::TrafficTLS13)
}

pub static TRAFFIC_TLS13: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::ApplicationData, ContentType::Handshake],
    handshake_types: &[HandshakeType::NewSessionTicket]
  },
  handle: handle_traffic_tls13
};
