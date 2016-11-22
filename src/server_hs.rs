use msgs::enums::{ContentType, HandshakeType, ProtocolVersion};
use msgs::enums::{Compression, NamedGroup, ECPointFormat, CipherSuite};
use msgs::enums::{ExtensionType, AlertDescription};
use msgs::enums::{ClientCertificateType, SignatureScheme};
use msgs::message::{Message, MessagePayload};
use msgs::base::Payload;
use msgs::handshake::{HandshakePayload, SupportedSignatureSchemes};
use msgs::handshake::{HandshakeMessagePayload, ServerHelloPayload, Random};
use msgs::handshake::{ClientHelloPayload, ServerExtension, SessionID};
use msgs::handshake::ConvertProtocolNameList;
use msgs::handshake::{NamedGroups, SupportedGroups, ClientExtension};
use msgs::handshake::{ECPointFormatList, SupportedPointFormats};
use msgs::handshake::{ServerECDHParams, DigitallySignedStruct};
use msgs::handshake::{ServerKeyExchangePayload, ECDHEServerKeyExchange};
use msgs::handshake::{CertificateRequestPayload, NewSessionTicketPayload};
use msgs::handshake::{HelloRetryRequest, HelloRetryExtension, KeyShareEntry};
use msgs::handshake::{CertificatePayloadTLS13, CertificateEntry};
use msgs::handshake::SupportedMandatedSignatureSchemes;
use msgs::ccs::ChangeCipherSpecPayload;
use msgs::codec::Codec;
use msgs::persist;
use session::{SessionSecrets, MessageCipherChange};
use cipher::MessageCipher;
use server::{ServerSessionImpl, ConnState};
use key_schedule::{KeySchedule, SecretKind};
use suites;
use sign;
use verify;
use util;
use error::TLSError;
use handshake::Expectation;

use std::sync::Arc;

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

pub type HandleFunction = fn(&mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError>;

/* These are effectively operations on the ServerSessionImpl, variant on the
 * connection state. They must not have state of their own -- so they're
 * functions rather than a trait. */
pub struct Handler {
  pub expect: Expectation,
  pub handle: HandleFunction
}

fn process_extensions(sess: &mut ServerSessionImpl, hello: &ClientHelloPayload)
  -> Result<Vec<ServerExtension>, TLSError> {
  let mut ret = Vec::new();

  /* ALPN */
  let our_protocols = &sess.config.alpn_protocols;
  let maybe_their_protocols = hello.get_alpn_extension();
  if let Some(their_protocols) = maybe_their_protocols {
    let their_proto_strings = their_protocols.to_strings();

    if their_proto_strings.contains(&"".to_string()) {
      return Err(TLSError::PeerMisbehavedError("client offered empty ALPN protocol".to_string()));
    }

    sess.alpn_protocol = util::first_in_both(&our_protocols, &their_proto_strings);
    match sess.alpn_protocol {
      Some(ref selected_protocol) => {
        info!("Chosen ALPN protocol {:?}", selected_protocol);
        ret.push(ServerExtension::make_alpn(selected_protocol.clone()))
      },
      _ => {}
    };
  }

  /* SNI */
  if hello.get_sni_extension().is_some() {
    ret.push(ServerExtension::ServerNameAcknowledgement);
  }

  if !sess.common.is_tls13 {
    /* Renegotiation.
     * (We don't do reneg at all, but would support the secure version if we did.) */
    let secure_reneg_offered =
      hello.find_extension(ExtensionType::RenegotiationInfo).is_some() ||
      hello.cipher_suites.contains(&CipherSuite::TLS_EMPTY_RENEGOTIATION_INFO_SCSV);

    if secure_reneg_offered {
      ret.push(ServerExtension::make_empty_renegotiation_info());
    }

    /* Tickets:
     * If we get any SessionTicket extension and have tickets enabled,
     * we send an ack. */
    if hello.find_extension(ExtensionType::SessionTicket).is_some() &&
      sess.config.ticketer.enabled() {
      sess.handshake_data.send_ticket = true;
      ret.push(ServerExtension::SessionTicketAcknowledgement);
    }
  }

  Ok(ret)
}

fn emit_server_hello(sess: &mut ServerSessionImpl, hello: &ClientHelloPayload) -> Result<(), TLSError> {
  let extensions = try!(process_extensions(sess, hello));

  if sess.handshake_data.session_id.is_empty() {
    let sessid = sess.config.session_storage.lock().unwrap()
      .generate();
    sess.handshake_data.session_id = sessid;
  }

  let sh = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ServerHello,
        payload: HandshakePayload::ServerHello(
          ServerHelloPayload {
            server_version: ProtocolVersion::TLSv1_2,
            random: Random::from_slice(&sess.handshake_data.randoms.server),
            session_id: sess.handshake_data.session_id.clone(),
            cipher_suite: sess.common.get_suite().suite,
            compression_method: Compression::Null,
            extensions: extensions
          }
        )
      }
    )
  };

  debug!("sending server hello {:?}", sh);
  sess.handshake_data.transcript.add_message(&sh);
  sess.common.send_msg(sh, false);
  Ok(())
}

fn emit_certificate(sess: &mut ServerSessionImpl) {
  let cert_chain = sess.handshake_data.server_cert_chain.as_ref().unwrap().clone();

  let c = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::Certificate,
        payload: HandshakePayload::Certificate(cert_chain)
      }
    )
  };

  sess.handshake_data.transcript.add_message(&c);
  sess.common.send_msg(c, false);
}

fn emit_server_kx(sess: &mut ServerSessionImpl,
                  sigscheme: SignatureScheme,
                  group: &NamedGroup,
                  signer: Arc<Box<sign::Signer + Send + Sync>>) -> Result<(), TLSError> {
  let kx = try!({
    let scs = sess.common.get_suite();
    scs.start_server_kx(*group)
      .ok_or_else(|| TLSError::PeerMisbehavedError("key exchange failed".to_string()))
  });
  let secdh = ServerECDHParams::new(group, &kx.pubkey);

  let mut msg = Vec::new();
  msg.extend(&sess.handshake_data.randoms.client);
  msg.extend(&sess.handshake_data.randoms.server);
  secdh.encode(&mut msg);

  let sig = try!(
    signer.sign(sigscheme, &msg)
    .map_err(|_| TLSError::General("signing failed".to_string()))
  );

  let skx = ServerKeyExchangePayload::ECDHE(
    ECDHEServerKeyExchange {
      params: secdh,
      dss: DigitallySignedStruct::new(sigscheme, sig)
    }
  );

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ServerKeyExchange,
        payload: HandshakePayload::ServerKeyExchange(skx)
      }
    )
  };

  sess.handshake_data.kx_data = Some(kx);
  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, false);
  Ok(())
}

fn emit_certificate_req(sess: &mut ServerSessionImpl) {
  if !sess.config.client_auth_offer {
    return;
  }

  let names = sess.config.client_auth_roots.get_subjects();

  let cr = CertificateRequestPayload {
    certtypes: vec![ ClientCertificateType::RSASign,
                     ClientCertificateType::ECDSASign ],
    sigschemes: SupportedSignatureSchemes::supported_verify(),
    canames: names
  };

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::CertificateRequest,
        payload: HandshakePayload::CertificateRequest(cr)
      }
    )
  };

  debug!("Sending CertificateRequest {:?}", m);
  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, false);
  sess.handshake_data.doing_client_auth = true;
}

fn emit_server_hello_done(sess: &mut ServerSessionImpl) {
  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ServerHelloDone,
        payload: HandshakePayload::ServerHelloDone
      }
    )
  };

  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, false);
}

fn incompatible(sess: &mut ServerSessionImpl, why: &str) -> TLSError {
  sess.common.send_fatal_alert(AlertDescription::HandshakeFailure);
  TLSError::PeerIncompatibleError(why.to_string())
}

fn start_resumption(sess: &mut ServerSessionImpl,
                    client_hello: &ClientHelloPayload,
                    id: &SessionID,
                    resumedata: persist::ServerSessionValue) -> Result<ConnState, TLSError> {
  info!("Resuming session");

  /* The RFC underspecifies this case.  Reject it, because someone's going to be
   * disappointed. */
  if sess.common.get_suite().suite != resumedata.cipher_suite {
    return Err(TLSError::PeerMisbehavedError("client varied ciphersuite over resumption".to_string()));
  }

  sess.handshake_data.session_id = id.clone();
  try!(emit_server_hello(sess, client_hello));

  let hashalg = sess.common.get_suite().get_hash();
  sess.secrets = Some(SessionSecrets::new_resume(&sess.handshake_data.randoms,
                                                 hashalg,
                                                 &resumedata.master_secret.0));
  sess.start_encryption_tls12();
  sess.handshake_data.valid_client_cert_chain = resumedata.client_cert_chain;
  sess.handshake_data.doing_resume = true;

  emit_ticket(sess);
  emit_ccs(sess);
  emit_finished(sess);
  return Ok(ConnState::ExpectCCS);
}

fn emit_server_hello_tls13(sess: &mut ServerSessionImpl,
                           share: &KeyShareEntry) -> Result<(), TLSError> {
  let mut extensions = Vec::new();

  /* Do key exchange */
  let kxr = try!(
    suites::KeyExchange::start_ecdhe(share.group)
      .and_then(|kx| kx.complete(&share.payload.0))
      .ok_or_else(|| TLSError::PeerMisbehavedError("key exchange failed".to_string()))
  );

  let kse = KeyShareEntry::new(share.group, &kxr.pubkey);
  extensions.push(ServerExtension::KeyShare(kse));

  let sh = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::ServerHello,
        payload: HandshakePayload::ServerHello(
          ServerHelloPayload {
            server_version: ProtocolVersion::Unknown(0x7f12),
            random: Random::from_slice(&sess.handshake_data.randoms.server),
            session_id: SessionID::empty(),
            cipher_suite: sess.common.get_suite().suite,
            compression_method: Compression::Null,
            extensions: extensions
          }
        )
      }
    )
  };

  debug!("sending server hello {:?}", sh);
  sess.handshake_data.transcript.add_message(&sh);
  sess.common.send_msg(sh, false);

  /* Start key schedule */
  let suite = sess.common.get_suite();
  let mut key_schedule = KeySchedule::new(suite.get_hash());
  key_schedule.input_empty();
  key_schedule.input_secret(&kxr.premaster_secret);

  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let write_key = key_schedule.derive(SecretKind::ServerHandshakeTrafficSecret, &handshake_hash);
  let read_key = key_schedule.derive(SecretKind::ClientHandshakeTrafficSecret, &handshake_hash);
  sess.common.set_message_cipher(MessageCipher::new_tls13(suite, &write_key, &read_key),
                                 MessageCipherChange::BothNew);
  key_schedule.current_client_traffic_secret = read_key;
  key_schedule.current_server_traffic_secret = write_key;
  sess.common.set_key_schedule(key_schedule);

  Ok(())
}

fn emit_hello_retry_request(sess: &mut ServerSessionImpl, group: NamedGroup) {
  let mut req = HelloRetryRequest {
    server_version: ProtocolVersion::Unknown(0x7f12),
    extensions: Vec::new()
  };

  req.extensions.push(HelloRetryExtension::KeyShare(group));

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::HelloRetryRequest,
        payload: HandshakePayload::HelloRetryRequest(req)
      }
    )
  };

  sess.common.send_msg(m, false);
}

fn emit_encrypted_extensions(sess: &mut ServerSessionImpl,
                             hello: &ClientHelloPayload) -> Result<(), TLSError> {
  let encrypted_exts = try!(process_extensions(sess, hello));
  let ee = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::EncryptedExtensions,
        payload: HandshakePayload::EncryptedExtensions(encrypted_exts)
      }
    )
  };

  debug!("sending encrypted extensions {:?}", ee);
  sess.handshake_data.transcript.add_message(&ee);
  sess.common.send_msg(ee, true);
  Ok(())
}

fn emit_certificate_tls13(sess: &mut ServerSessionImpl) {
  let mut cert_body = CertificatePayloadTLS13::new();

  for cert in sess.handshake_data.server_cert_chain.as_ref().unwrap() {
    let entry = CertificateEntry {
      cert: cert.clone(),
      exts: Vec::new()
    };

    cert_body.list.push(entry);
  }

  let c = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::Certificate,
        payload: HandshakePayload::CertificateTLS13(cert_body)
      }
    )
  };

  debug!("sending certificate {:?}", c);
  sess.handshake_data.transcript.add_message(&c);
  sess.common.send_msg(c, true);
}

fn emit_certificate_verify_tls13(sess: &mut ServerSessionImpl,
                                 schemes: &SupportedSignatureSchemes,
                                 signer: &Arc<Box<sign::Signer + Send + Sync>>) -> Result<(), TLSError> {
  let mut message = Vec::new();
  message.resize(64, 0x20u8);
  message.extend_from_slice(b"TLS 1.3, server CertificateVerify\x00");
  message.extend_from_slice(&sess.handshake_data.transcript.get_current_hash());

  let scheme = try!(signer.choose_scheme(schemes)
    .ok_or_else(|| TLSError::General("no overlapping sigschemes".to_string())));

  let sig = try!(signer.sign(scheme, &message)
    .map_err(|_| TLSError::General("cannot sign".to_string())));

  let cv = DigitallySignedStruct::new(scheme, sig);

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_3,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::CertificateVerify,
        payload: HandshakePayload::CertificateVerify(cv)
      }
    )
  };

  debug!("sending certificate-verify {:?}", m);
  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, true);
  Ok(())
}

fn emit_finished_tls13(sess: &mut ServerSessionImpl) {
  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let verify_data = sess.common.get_key_schedule()
    .sign_verify_data(SecretKind::ServerHandshakeTrafficSecret, &handshake_hash);
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

  debug!("sending finished {:?}", m);
  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, true);
}

fn handle_client_hello_tls13(sess: &mut ServerSessionImpl,
                             client_hello: &ClientHelloPayload,
                             signer: &Arc<Box<sign::Signer + Send + Sync>>) -> Result<ConnState, TLSError> {
  let groups_ext = try!(client_hello.get_namedgroups_extension()
    .ok_or_else(|| incompatible(sess, "client didn't describe groups")));

  let sigschemes_ext = try!(client_hello.get_sigalgs_extension()
    .ok_or_else(|| incompatible(sess, "client didn't describe sigschemes")));

  let shares_ext = try!(client_hello.get_keyshare_extension()
    .ok_or_else(|| incompatible(sess, "client didn't send keyshares")));

  let share_groups: Vec<NamedGroup> = shares_ext.iter()
    .map(|share| share.group)
    .collect();

  let chosen_group = util::first_in_both(&NamedGroups::supported(), &share_groups);
  if chosen_group.is_none() {
    /* We don't have a suitable key share.  Choose a suitable group and
     * send a HelloRetryRequest. */
    let retry_group_maybe = util::first_in_both(&NamedGroups::supported(), groups_ext);

    if let Some(group) = retry_group_maybe {
      emit_hello_retry_request(sess, group);
      return Ok(ConnState::ExpectClientHello);
    } else {
      return Err(TLSError::PeerIncompatibleError("no kx group overlap with client".to_string()));
    }
  }

  let chosen_group = chosen_group.unwrap();
  let chosen_share = shares_ext.iter()
    .find(|share| share.group == chosen_group)
    .unwrap();

  try!(emit_server_hello_tls13(sess, chosen_share));
  try!(emit_encrypted_extensions(sess, client_hello));
  emit_certificate_tls13(sess);
  try!(emit_certificate_verify_tls13(sess, &sigschemes_ext, signer));
  emit_finished_tls13(sess);

  return Ok(ConnState::ExpectFinishedTLS13);
}

fn handle_client_hello(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let client_hello = extract_handshake!(m, HandshakePayload::ClientHello).unwrap();

  if client_hello.client_version.get_u16() < ProtocolVersion::TLSv1_2.get_u16() {
    sess.common.send_fatal_alert(AlertDescription::ProtocolVersion);
    return Err(TLSError::PeerIncompatibleError("client does not support TLSv1_2".to_string()));
  }

  if !client_hello.compression_methods.contains(&Compression::Null) {
    sess.common.send_fatal_alert(AlertDescription::IllegalParameter);
    return Err(TLSError::PeerIncompatibleError("client did not offer Null compression".to_string()));
  }

  if client_hello.has_duplicate_extension() {
    sess.common.send_fatal_alert(AlertDescription::DecodeError);
    return Err(TLSError::PeerMisbehavedError("client sent duplicate extensions".to_string()));
  }

  /* Common to TLS1.2 and TLS1.3: ciphersuite and certificate selection. */
  debug!("we got a clienthello {:?}", client_hello);

  let default_sigschemes_ext = SupportedSignatureSchemes::default();

  let sni_ext = client_hello.get_sni_extension();
  let sigschemes_ext = client_hello.get_sigalgs_extension()
    .unwrap_or(&default_sigschemes_ext);

  debug!("sni {:?}", sni_ext);
  debug!("sig schemes {:?}", sigschemes_ext);

  /* Choose a certificate. */
  let maybe_cert_key = sess.config.cert_resolver.resolve(sni_ext, sigschemes_ext);
  if maybe_cert_key.is_err() {
    sess.common.send_fatal_alert(AlertDescription::AccessDenied);
    return Err(TLSError::General("no server certificate chain resolved".to_string()));
  }
  let (cert_chain, private_key) = maybe_cert_key.unwrap();

  /* Reduce our supported ciphersuites by the certificate.
   * (no-op for TLS1.3) */
  let ciphersuites_suitable_for_cert = suites::reduce_given_sigalg(&sess.config.ciphersuites,
                                                                   &private_key.algorithm());
  sess.handshake_data.server_cert_chain = Some(cert_chain);

  let maybe_ciphersuite = if sess.config.ignore_client_order {
    suites::choose_ciphersuite_preferring_server(&client_hello.cipher_suites,
                                                 &ciphersuites_suitable_for_cert)
  } else {
    suites::choose_ciphersuite_preferring_client(&client_hello.cipher_suites,
                                                 &ciphersuites_suitable_for_cert)
  };

  if maybe_ciphersuite.is_none() {
    return Err(incompatible(sess, "no ciphersuites in common"));
  }

  info!("decided upon suite {:?}", maybe_ciphersuite.as_ref().unwrap());
  sess.common.set_suite(maybe_ciphersuite.unwrap());

  /* Start handshake hash. */
  sess.handshake_data.transcript.start_hash(sess.common.get_suite().get_hash());
  sess.handshake_data.transcript.add_message(&m);

  /* Are we doing TLS1.3? */
  let maybe_versions_ext = client_hello.get_versions_extension();
  if let Some(versions) = maybe_versions_ext {
    if versions.contains(&ProtocolVersion::Unknown(0x7f12)) {
      sess.common.is_tls13 = true;
      return handle_client_hello_tls13(sess, &client_hello, &private_key);
    }
  }

  /* -- TLS1.2 only from hereon in -- */
  /* Save their Random. */
  client_hello.random.write_slice(&mut sess.handshake_data.randoms.client);

  let groups_ext = try!(client_hello.get_namedgroups_extension()
                          .ok_or_else(|| incompatible(sess, "client didn't describe groups")));
  let ecpoints_ext = try!(client_hello.get_ecpoints_extension()
                          .ok_or_else(|| incompatible(sess, "client didn't describe ec points")));

  debug!("namedgroups {:?}", groups_ext);
  debug!("ecpoints {:?}", ecpoints_ext);

  if !ecpoints_ext.contains(&ECPointFormat::Uncompressed) {
    sess.common.send_fatal_alert(AlertDescription::IllegalParameter);
    return Err(TLSError::PeerIncompatibleError("client didn't support uncompressed ec points".to_string()));
  }

  /* -- Check for resumption --
   * We can do this either by (in order of preference):
   * 1. receiving a ticket that decrypts
   * 2. receiving a sessionid that is in our cache
   *
   * If we receive a ticket, the sessionid won't be in our
   * cache, so don't check.
   *
   * If either works, we end up with a ServerSessionValue
   * which is passed to start_resumption and concludes
   * our handling of the ClientHello.
   */
  let mut ticket_received = false;

  if let Some(ticket_ext) = client_hello.get_ticket_extension() {
    match ticket_ext {
      &ClientExtension::SessionTicketOffer(ref ticket) => {
        ticket_received = true;
        info!("Ticket received");

        let maybe_resume = sess.config.ticketer.decrypt(&ticket.0)
          .and_then(|plain| persist::ServerSessionValue::read_bytes(&plain));

        if maybe_resume.is_some() {
          return start_resumption(sess,
                                  client_hello,
                                  &client_hello.session_id,
                                  maybe_resume.unwrap());
        } else {
          info!("Ticket didn't decrypt");
        }
      }

      // eg ClientExtension::SessionTicketRequest
      _ => (),
    }
  }

  /* Perhaps resume?  If we received a ticket, the sessionid
   * does not correspond to a real session. */
  if !client_hello.session_id.is_empty() && !ticket_received {
    let maybe_resume = {
      let persist = sess.config.session_storage.lock().unwrap();
      persist.get(&client_hello.session_id)
    }.and_then(|x| persist::ServerSessionValue::read_bytes(&x));

    if maybe_resume.is_some() {
      return start_resumption(sess,
                              client_hello,
                              &client_hello.session_id,
                              maybe_resume.unwrap());
    }
  }

  /* Now we have chosen a ciphersuite, we can make kx decisions. */
  let sigscheme = try!(
    sess.common.get_suite()
      .resolve_sig_scheme(sigschemes_ext)
      .ok_or_else(|| incompatible(sess, "no supported sig scheme"))
  );
  let group = try!(
    util::first_in_both(NamedGroups::supported().as_slice(),
                        groups_ext.as_slice())
      .ok_or_else(|| incompatible(sess, "no supported group"))
  );
  let ecpoint = try!(
    util::first_in_both(ECPointFormatList::supported().as_slice(),
                        ecpoints_ext.as_slice())
      .ok_or_else(|| incompatible(sess, "no supported point format"))
  );

  debug_assert_eq!(ecpoint, ECPointFormat::Uncompressed);

  try!(emit_server_hello(sess, client_hello));
  emit_certificate(sess);
  try!(emit_server_kx(sess, sigscheme, &group, private_key));
  emit_certificate_req(sess);
  emit_server_hello_done(sess);

  if sess.handshake_data.doing_client_auth {
    Ok(ConnState::ExpectCertificate)
  } else {
    Ok(ConnState::ExpectClientKX)
  }
}

pub static EXPECT_CLIENT_HELLO: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::ClientHello]
  },
  handle: handle_client_hello
};

/* --- Process client's Certificate for client auth --- */
fn handle_certificate(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  sess.handshake_data.transcript.add_message(&m);
  let cert_chain = extract_handshake!(m, HandshakePayload::Certificate).unwrap();

  if cert_chain.is_empty() && !sess.config.client_auth_mandatory {
    info!("client auth requested but no certificate supplied");
    sess.handshake_data.doing_client_auth = false;
    sess.handshake_data.transcript.abandon_client_auth();
    return Ok(ConnState::ExpectClientKX);
  }

  debug!("certs {:?}", cert_chain);

  try!(
    verify::verify_client_cert(&sess.config.client_auth_roots,
                               &cert_chain)
  );

  sess.handshake_data.valid_client_cert_chain = Some(cert_chain.clone());
  Ok(ConnState::ExpectClientKX)
}

pub static EXPECT_CERTIFICATE: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::Certificate]
  },
  handle: handle_certificate
};

/* --- Process client's KeyExchange --- */
fn handle_client_kx(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let client_kx = extract_handshake!(m, HandshakePayload::ClientKeyExchange).unwrap();
  sess.handshake_data.transcript.add_message(&m);

  /* Complete key agreement, and set up encryption with the
   * resulting premaster secret. */
  let kx = sess.handshake_data.kx_data.take().unwrap();
  let kxd = try!(
    kx.server_complete(&client_kx.0)
    .ok_or_else(|| TLSError::PeerMisbehavedError("key exchange completion failed".to_string()))
  );

  let hashalg = sess.common.get_suite().get_hash();
  sess.secrets = Some(SessionSecrets::new(&sess.handshake_data.randoms,
                                          hashalg,
                                          &kxd.premaster_secret));
  sess.start_encryption_tls12();

  if sess.handshake_data.doing_client_auth {
    Ok(ConnState::ExpectCertificateVerify)
  } else {
    Ok(ConnState::ExpectCCS)
  }
}

pub static EXPECT_CLIENT_KX: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::ClientKeyExchange]
  },
  handle: handle_client_kx
};

/* --- Process client's certificate proof --- */
fn handle_certificate_verify(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let rc = {
    let sig = extract_handshake!(m, HandshakePayload::CertificateVerify).unwrap();
    let certs = sess.handshake_data.valid_client_cert_chain.as_ref().unwrap();
    let handshake_msgs = sess.handshake_data.transcript.take_handshake_buf();

    verify::verify_signed_struct(&handshake_msgs, &certs[0], &sig)
  };

  if rc.is_err() {
    sess.common.send_fatal_alert(AlertDescription::AccessDenied);
    return Err(rc.unwrap_err());
  } else {
    debug!("client CertificateVerify OK");
  }

  sess.handshake_data.transcript.add_message(&m);
  Ok(ConnState::ExpectCCS)
}

pub static EXPECT_CERTIFICATE_VERIFY: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::CertificateVerify]
  },
  handle: handle_certificate_verify
};

/* --- Process client's ChangeCipherSpec --- */
fn handle_ccs(sess: &mut ServerSessionImpl, _m: Message) -> Result<ConnState, TLSError> {
  /* CCS should not be received interleaved with fragmented handshake-level
   * message. */
  if !sess.common.handshake_joiner.is_empty() {
    warn!("CCS received interleaved with fragmented handshake");
    return Err(TLSError::InappropriateMessage {
      expect_types: vec![ ContentType::Handshake ],
      got_type: ContentType::ChangeCipherSpec
    });
  }

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

/* --- Process client's Finished --- */
fn emit_ticket(sess: &mut ServerSessionImpl) {
  if !sess.handshake_data.send_ticket {
    return;
  }

  /* If we can't produce a ticket for some reason, we can't
   * report an error. Send an empty one. */
  let plain = get_server_session_value(sess).get_encoding();
  let ticket = sess.config.ticketer.encrypt(&plain)
    .unwrap_or_else(Vec::new);
  let ticket_lifetime = sess.config.ticketer.get_lifetime();

  let m = Message {
    typ: ContentType::Handshake,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::Handshake(
      HandshakeMessagePayload {
        typ: HandshakeType::NewSessionTicket,
        payload: HandshakePayload::NewSessionTicket(NewSessionTicketPayload::new(ticket_lifetime, ticket))
      }
    )
  };

  sess.handshake_data.transcript.add_message(&m);
  sess.common.send_msg(m, false);
}

fn emit_ccs(sess: &mut ServerSessionImpl) {
  let m = Message {
    typ: ContentType::ChangeCipherSpec,
    version: ProtocolVersion::TLSv1_2,
    payload: MessagePayload::ChangeCipherSpec(ChangeCipherSpecPayload {})
  };

  sess.common.send_msg(m, false);
  sess.common.we_now_encrypting();
}

fn emit_finished(sess: &mut ServerSessionImpl) {
  let vh = sess.handshake_data.transcript.get_current_hash();
  let verify_data = sess.secrets.as_ref().unwrap().server_verify_data(&vh);
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

fn get_server_session_value(sess: &ServerSessionImpl) -> persist::ServerSessionValue {
  let scs = sess.common.get_suite();
  let client_certs = &sess.handshake_data.valid_client_cert_chain;

  persist::ServerSessionValue::new(&scs.suite,
                                   sess.secrets.as_ref().unwrap().get_master_secret(),
                                   client_certs)
}

fn handle_finished(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let finished = extract_handshake!(m, HandshakePayload::Finished).unwrap();

  let vh = sess.handshake_data.transcript.get_current_hash();
  let expect_verify_data = sess.secrets.as_ref().unwrap().client_verify_data(&vh);

  use ring;
  try!(
    ring::constant_time::verify_slices_are_equal(&expect_verify_data, &finished.0)
      .map_err(|_| { error!("Finished wrong"); TLSError::DecryptError })
  );

  /* Save session, perhaps */
  if !sess.handshake_data.doing_resume && !sess.handshake_data.session_id.is_empty() {
    let value = get_server_session_value(sess);

    let mut persist = sess.config.session_storage.lock().unwrap();
    if persist.put(&sess.handshake_data.session_id, value.get_encoding()) {
      info!("Session saved");
    } else {
      info!("Session not saved");
    }
  }

  /* Send our CCS and Finished. */
  sess.handshake_data.transcript.add_message(&m);
  if !sess.handshake_data.doing_resume {
    emit_ticket(sess);
    emit_ccs(sess);
    emit_finished(sess);
  }
  Ok(ConnState::Traffic)
}

pub static EXPECT_FINISHED: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::Finished]
  },
  handle: handle_finished
};

fn handle_finished_tls13(sess: &mut ServerSessionImpl, m: Message) -> Result<ConnState, TLSError> {
  let finished = extract_handshake!(m, HandshakePayload::Finished).unwrap();

  let handshake_hash = sess.handshake_data.transcript.get_current_hash();
  let expect_verify_data = sess.common.get_key_schedule()
    .sign_verify_data(SecretKind::ClientHandshakeTrafficSecret, &handshake_hash);

  use ring;
  try!(
    ring::constant_time::verify_slices_are_equal(&expect_verify_data, &finished.0)
      .map_err(|_| { error!("Finished wrong"); TLSError::DecryptError })
  );

  /* nb. future derivations include Client Finished, but not the
   * main application data keying. */
  sess.handshake_data.transcript.add_message(&m);

  sess.common.get_mut_key_schedule().input_empty();
  let (write_key, read_key) = {
    let key_schedule = sess.common.get_key_schedule();

    (key_schedule.derive(SecretKind::ServerApplicationTrafficSecret, &handshake_hash),
     key_schedule.derive(SecretKind::ClientApplicationTrafficSecret, &handshake_hash))
  };

  let suite = sess.common.get_suite();
  sess.common.set_message_cipher(MessageCipher::new_tls13(suite, &write_key, &read_key),
                                 MessageCipherChange::BothNew);

  {
    let key_schedule = sess.common.get_mut_key_schedule();
    key_schedule.current_server_traffic_secret = write_key;
    key_schedule.current_client_traffic_secret = read_key;
  }

  Ok(ConnState::Traffic) // TODO: accept keyupdates
}

pub static EXPECT_FINISHED_TLS13: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::Handshake],
    handshake_types: &[HandshakeType::Finished]
  },
  handle: handle_finished_tls13
};

/* --- Process traffic --- */
fn handle_traffic(sess: &mut ServerSessionImpl, mut m: Message) -> Result<ConnState, TLSError> {
  sess.common.take_received_plaintext(m.take_opaque_payload().unwrap());
  Ok(ConnState::Traffic)
}

pub static TRAFFIC: Handler = Handler {
  expect: Expectation {
    content_types: &[ContentType::ApplicationData],
    handshake_types: &[]
  },
  handle: handle_traffic
};
