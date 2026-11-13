//! Runs every vector in `spec/vectors/cluster.json` against this crate: a message
//! vector decodes to its fields on its stream and encodes back to its bytes, or is
//! rejected for the named reason; a members vector parses to its members or is
//! refused; an arithmetic vector's fields are computed from its inputs
//! (`spec/README.md`, "Conventions"; ADR 0058). `just conformance` runs this test
//! alone; `just test` runs it with the rest.

use cavalry_proto::cluster::{
    AppointedCanvass, Ballot, Candidate, CanvassAction, CanvassTerms, ChangeType, CloseReason,
    ClusterAction, ClusterError, CommitPosition, EMPTY_MEMBER, EventCode, LogReport, MAX_MEMBERS,
    Member, MemberPosition, Message, NewLeadershipTermAction, NextFields, RecordedTerms, ReplayEnd,
    ReplicationStop, SnapshotMark, Stream, TermFields, TermSpan, TimeUnit, VERSION, Vote,
    VoteAnswer, VoteRequest, answered_term, appointed_canvass, appointed_leader, ballot, candidate,
    canvass_position_action, commit_position, entry_position, leader_ready, log_at_least,
    new_leadership_term_action, next_fields, parse_members, quorum, replay_end, replication_record,
    replication_stop, request_vote,
};
use serde_json::{Map, Value, json};

const VECTORS: &str = include_str!("../../../spec/vectors/cluster.json");

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector bytes are hex"))
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("the vectors' strings are UTF-8")
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stream(name: &str) -> Stream {
    match name {
        "log" => Stream::Log,
        "consensus" => Stream::Consensus,
        "ingress" => Stream::Ingress,
        "egress" => Stream::Egress,
        "snapshot" => Stream::Snapshot,
        other => panic!("unknown stream {other}"),
    }
}

/// The decoded message as the vector's `fields` object: the spec's field names, the
/// variable ASCII fields as text, the data fields and the payload as hex.
fn fields(message: &Message<'_>) -> Value {
    match *message {
        Message::SessionMessage {
            leadership_term_id,
            cluster_session_id,
            timestamp,
            payload,
        } => json!({
            "leadership_term_id": leadership_term_id, "cluster_session_id": cluster_session_id,
            "timestamp": timestamp, "payload": to_hex(payload),
        }),
        Message::SessionEvent {
            cluster_session_id,
            correlation_id,
            leadership_term_id,
            leader_member_id,
            code,
            version,
            leader_heartbeat_timeout_ns,
            detail,
        } => json!({
            "cluster_session_id": cluster_session_id, "correlation_id": correlation_id,
            "leadership_term_id": leadership_term_id, "leader_member_id": leader_member_id,
            "code": code.code(), "version": version,
            "leader_heartbeat_timeout_ns": leader_heartbeat_timeout_ns, "detail": text(detail),
        }),
        Message::SessionConnect {
            correlation_id,
            response_stream_id,
            version,
            response_channel,
            encoded_credentials,
            client_info,
        } => json!({
            "correlation_id": correlation_id, "response_stream_id": response_stream_id,
            "version": version, "response_channel": text(response_channel),
            "encoded_credentials": to_hex(encoded_credentials), "client_info": text(client_info),
        }),
        Message::SessionClose {
            leadership_term_id,
            cluster_session_id,
        }
        | Message::SessionKeepalive {
            leadership_term_id,
            cluster_session_id,
        } => json!({
            "leadership_term_id": leadership_term_id, "cluster_session_id": cluster_session_id,
        }),
        Message::NewLeaderEvent {
            leadership_term_id,
            cluster_session_id,
            leader_member_id,
            ingress_endpoints,
        } => json!({
            "leadership_term_id": leadership_term_id, "cluster_session_id": cluster_session_id,
            "leader_member_id": leader_member_id, "ingress_endpoints": text(ingress_endpoints),
        }),
        Message::TimerEvent {
            leadership_term_id,
            correlation_id,
            timestamp,
        } => json!({
            "leadership_term_id": leadership_term_id, "correlation_id": correlation_id,
            "timestamp": timestamp,
        }),
        Message::SessionOpenEvent {
            leadership_term_id,
            correlation_id,
            cluster_session_id,
            timestamp,
            response_stream_id,
            response_channel,
            encoded_principal,
        } => json!({
            "leadership_term_id": leadership_term_id, "correlation_id": correlation_id,
            "cluster_session_id": cluster_session_id, "timestamp": timestamp,
            "response_stream_id": response_stream_id, "response_channel": text(response_channel),
            "encoded_principal": to_hex(encoded_principal),
        }),
        Message::SessionCloseEvent {
            leadership_term_id,
            cluster_session_id,
            timestamp,
            close_reason,
        } => json!({
            "leadership_term_id": leadership_term_id, "cluster_session_id": cluster_session_id,
            "timestamp": timestamp, "close_reason": close_reason.code(),
        }),
        Message::ClusterAction {
            leadership_term_id,
            log_position,
            timestamp,
            action,
            flags,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "timestamp": timestamp, "action": action.code(), "flags": flags,
        }),
        Message::NewLeadershipTermEvent {
            leadership_term_id,
            log_position,
            timestamp,
            term_base_log_position,
            leader_member_id,
            log_session_id,
            time_unit,
            app_version,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "timestamp": timestamp, "term_base_log_position": term_base_log_position,
            "leader_member_id": leader_member_id, "log_session_id": log_session_id,
            "time_unit": time_unit.code(), "app_version": app_version,
        }),
        Message::MembershipChangeEvent {
            leadership_term_id,
            log_position,
            timestamp,
            leader_member_id,
            cluster_size,
            change_type,
            member_id,
            cluster_members,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "timestamp": timestamp, "leader_member_id": leader_member_id,
            "cluster_size": cluster_size, "change_type": change_type.code(),
            "member_id": member_id, "cluster_members": text(cluster_members),
        }),
        Message::CanvassPosition {
            log_leadership_term_id,
            log_position,
            leadership_term_id,
            follower_member_id,
            protocol_version,
        } => json!({
            "log_leadership_term_id": log_leadership_term_id, "log_position": log_position,
            "leadership_term_id": leadership_term_id, "follower_member_id": follower_member_id,
            "protocol_version": protocol_version,
        }),
        Message::RequestVote {
            log_leadership_term_id,
            log_position,
            candidate_term_id,
            candidate_member_id,
            protocol_version,
        } => json!({
            "log_leadership_term_id": log_leadership_term_id, "log_position": log_position,
            "candidate_term_id": candidate_term_id, "candidate_member_id": candidate_member_id,
            "protocol_version": protocol_version,
        }),
        Message::Vote {
            candidate_term_id,
            log_leadership_term_id,
            log_position,
            candidate_member_id,
            follower_member_id,
            vote,
        } => json!({
            "candidate_term_id": candidate_term_id, "log_leadership_term_id": log_leadership_term_id,
            "log_position": log_position, "candidate_member_id": candidate_member_id,
            "follower_member_id": follower_member_id, "vote": i32::from(vote),
        }),
        Message::NewLeadershipTerm {
            log_leadership_term_id,
            next_leadership_term_id,
            next_term_base_log_position,
            next_log_position,
            leadership_term_id,
            term_base_log_position,
            log_position,
            leader_recording_id,
            timestamp,
            leader_member_id,
            log_session_id,
            app_version,
            is_startup,
            commit_position,
        } => json!({
            "log_leadership_term_id": log_leadership_term_id,
            "next_leadership_term_id": next_leadership_term_id,
            "next_term_base_log_position": next_term_base_log_position,
            "next_log_position": next_log_position, "leadership_term_id": leadership_term_id,
            "term_base_log_position": term_base_log_position, "log_position": log_position,
            "leader_recording_id": leader_recording_id, "timestamp": timestamp,
            "leader_member_id": leader_member_id, "log_session_id": log_session_id,
            "app_version": app_version, "is_startup": i32::from(is_startup),
            "commit_position": commit_position,
        }),
        Message::AppendPosition {
            leadership_term_id,
            log_position,
            follower_member_id,
            flags,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "follower_member_id": follower_member_id, "flags": flags,
        }),
        Message::CommitPosition {
            leadership_term_id,
            log_position,
            leader_member_id,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "leader_member_id": leader_member_id,
        }),
        Message::CatchupPosition {
            leadership_term_id,
            log_position,
            follower_member_id,
            catchup_endpoint,
        } => json!({
            "leadership_term_id": leadership_term_id, "log_position": log_position,
            "follower_member_id": follower_member_id, "catchup_endpoint": text(catchup_endpoint),
        }),
        Message::StopCatchup {
            leadership_term_id,
            follower_member_id,
        } => json!({
            "leadership_term_id": leadership_term_id, "follower_member_id": follower_member_id,
        }),
        Message::SnapshotMarker {
            type_id,
            log_position,
            leadership_term_id,
            index,
            mark,
            time_unit,
            app_version,
        } => json!({
            "type_id": type_id, "log_position": log_position,
            "leadership_term_id": leadership_term_id, "index": index, "mark": mark.code(),
            "time_unit": time_unit.code(), "app_version": app_version,
        }),
    }
}

fn message_vector(name: &str, vector: &Value) {
    let on = stream(vector["stream"].as_str().expect("stream"));
    let bytes = hex(vector["bytes"].as_str().expect("bytes"));
    let result = Message::parse(on, &bytes);
    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        match result {
            Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
            Ok(message) => panic!("{name}: accepted, expected {reason}: {message:?}"),
        }
        return;
    }
    let message = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
    assert_eq!(
        message.template().name(),
        vector["template"].as_str().expect("template"),
        "{name}: template"
    );
    assert_eq!(fields(&message), vector["fields"], "{name}: fields");
    let mut buffer = vec![0u8; bytes.len()];
    let length = message
        .write(&mut buffer)
        .expect("a decoded message encodes");
    assert_eq!(length, message.encoded_length(), "{name}: encoded_length");
    assert_eq!(
        buffer[..length],
        bytes,
        "{name}: the fields encode to different bytes"
    );
    let short = message.write(&mut buffer[..length - 1]).unwrap_err();
    assert!(
        matches!(short, ClusterError::TooShort { .. }),
        "{name}: a short buffer is too-short"
    );
}

fn member_json(member: &Member<'_>) -> Value {
    let endpoint =
        |e: &cavalry_proto::cluster::Endpoint<'_>| json!({"host": text(e.host), "port": e.port});
    json!({
        "id": member.id, "ingress": endpoint(&member.ingress), "consensus": endpoint(&member.consensus),
        "log": endpoint(&member.log), "catchup": endpoint(&member.catchup), "archive": endpoint(&member.archive),
    })
}

fn members_vector(name: &str, vector: &Value) {
    let string = vector["string"].as_str().expect("string");
    let self_id = vector
        .get("self_id")
        .map(|id| i32::try_from(id.as_i64().expect("self_id")).expect("self_id fits"));
    let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
    let result = parse_members(string.as_bytes(), &mut members, self_id);
    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        match result {
            Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
            Ok(count) => panic!("{name}: accepted {count} members, expected {reason}"),
        }
        return;
    }
    let count = result.unwrap_or_else(|error| panic!("{name}: refused: {error}"));
    let parsed: Vec<Value> = members[..count].iter().map(member_json).collect();
    assert_eq!(Value::Array(parsed), vector["members"], "{name}: members");
}

fn int(value: &Value, name: &str) -> i64 {
    value[name].as_i64().unwrap_or_else(|| panic!("{name}"))
}

fn arithmetic_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let computed: Value = match vector["rule"].as_str().expect("rule") {
        "quorum" => {
            let members = usize::try_from(int(inputs, "members")).expect("members");
            json!({"quorum": quorum(members)})
        }
        "commit-position" => {
            let positions: Vec<MemberPosition> = inputs["positions"]
                .as_array()
                .expect("positions")
                .iter()
                .map(|p| MemberPosition {
                    position: int(p, "position"),
                    age_ns: u64::try_from(int(p, "age_ns")).expect("age"),
                    term: int(p, "term"),
                })
                .collect();
            let CommitPosition {
                quorum_position,
                commit_position: commit,
            } = commit_position(
                &positions,
                int(inputs, "leader_position"),
                int(inputs, "leader_term"),
                int(inputs, "term_base"),
                cavalry_proto::cluster::LEADER_HEARTBEAT_TIMEOUT_NS,
            );
            json!({"quorum_position": quorum_position, "commit_position": commit})
        }
        "compare-log" => json!({"at_least": log_at_least(
            int(inputs, "term"), int(inputs, "position"),
            int(inputs, "other_term"), int(inputs, "other_position"),
        )}),
        "ballot" => {
            let members = usize::try_from(int(inputs, "members")).expect("members");
            let mut votes = vec![None; members];
            for vote in inputs["votes"].as_array().expect("votes") {
                let member = usize::try_from(int(vote, "member")).expect("member");
                votes[member] = Some(Vote {
                    vote: vote["vote"].as_bool().expect("vote"),
                    term: int(vote, "term"),
                });
            }
            let mut heard = vec![false; members];
            for member in inputs["heard"].as_array().expect("heard") {
                let member =
                    usize::try_from(member.as_i64().expect("heard member")).expect("heard member");
                heard[member] = true;
            }
            let Ballot {
                leader,
                unanimous,
                lost,
            } = ballot(
                members,
                int(inputs, "candidate_term"),
                &votes,
                &heard,
                inputs["timeout_passed"].as_bool().expect("timeout_passed"),
            );
            json!({"leader": leader, "unanimous": unanimous, "lost": lost})
        }
        "candidate" => {
            let members = usize::try_from(int(inputs, "members")).expect("members");
            let reports: Vec<Option<LogReport>> = inputs["reports"]
                .as_array()
                .expect("reports")
                .iter()
                .map(|r| {
                    r.as_array().map(|pair| LogReport {
                        term: pair[0].as_i64().expect("term"),
                        position: pair[1].as_i64().expect("position"),
                    })
                })
                .collect();
            let own = inputs["own"].as_array().expect("own");
            let own = LogReport {
                term: own[0].as_i64().expect("term"),
                position: own[1].as_i64().expect("position"),
            };
            let Candidate {
                nominate,
                unanimous,
            } = candidate(
                members,
                &reports,
                own,
                inputs["deadline_passed"]
                    .as_bool()
                    .expect("deadline_passed"),
            );
            json!({"nominate": nominate, "unanimous": unanimous})
        }
        "replication-stop" => {
            let ReplicationStop {
                replicate,
                stop_position,
            } = replication_stop(
                int(inputs, "append_position"),
                int(inputs, "next_term_base_log_position"),
                int(inputs, "next_log_position"),
            );
            json!({"replicate": replicate, "stop_position": stop_position})
        }
        "new-leadership-term" => {
            let m = &inputs["message"];
            let NewLeadershipTermAction {
                action,
                truncate_to,
                stop_position,
                records,
            } = new_leadership_term_action(
                int(inputs, "log_leadership_term_id"),
                int(inputs, "append_position"),
                int(inputs, "candidate_term"),
                inputs["in_canvass"].as_bool().expect("in_canvass"),
                int(inputs, "own_base_above"),
                TermFields {
                    leadership_term_id: int(m, "leadership_term_id"),
                    log_leadership_term_id: int(m, "log_leadership_term_id"),
                    next_leadership_term_id: int(m, "next_leadership_term_id"),
                    next_term_base_log_position: int(m, "next_term_base_log_position"),
                    next_log_position: int(m, "next_log_position"),
                    term_base_log_position: int(m, "term_base_log_position"),
                },
            );
            let records = records.map(|(term, base)| json!([term, base]));
            json!({"action": action.name(), "truncate_to": truncate_to, "stop_position": stop_position, "records": records})
        }
        "request-vote" => {
            let log = inputs["log"].as_array().expect("log");
            let r = &inputs["request"];
            let VoteAnswer {
                vote,
                candidate_term,
            } = request_vote(
                int(inputs, "candidate_term"),
                LogReport {
                    term: log[0].as_i64().expect("term"),
                    position: log[1].as_i64().expect("position"),
                },
                u8::try_from(int(inputs, "state")).expect("state"),
                VoteRequest {
                    candidate_term_id: int(r, "candidate_term_id"),
                    log_leadership_term_id: int(r, "log_leadership_term_id"),
                    log_position: int(r, "log_position"),
                },
            );
            json!({"vote": vote, "candidate_term": candidate_term})
        }
        "canvass-position" => {
            let m = &inputs["message"];
            let action: CanvassAction = canvass_position_action(
                int(inputs, "leader_term"),
                u8::try_from(int(inputs, "state")).expect("state"),
                CanvassTerms {
                    leadership_term_id: int(m, "leadership_term_id"),
                    log_leadership_term_id: int(m, "log_leadership_term_id"),
                },
            );
            json!({"action": action.name()})
        }
        "answered-term" => {
            let terms: Vec<TermSpan> = inputs["terms"]
                .as_array()
                .expect("terms")
                .iter()
                .map(|t| TermSpan {
                    term: t[0].as_i64().expect("term"),
                    base: t[1].as_i64().expect("base"),
                    end: t[2].as_i64().expect("end"),
                })
                .collect();
            json!({"log_leadership_term_id": answered_term(&terms, int(inputs, "reported_term"))})
        }
        "next-fields" => {
            let terms: Vec<TermSpan> = inputs["terms"]
                .as_array()
                .expect("terms")
                .iter()
                .map(|t| TermSpan {
                    term: t[0].as_i64().expect("term"),
                    base: t[1].as_i64().expect("base"),
                    end: t[2].as_i64().expect("end"),
                })
                .collect();
            let NextFields {
                next_leadership_term_id,
                next_term_base_log_position,
                next_log_position,
            } = next_fields(
                &terms,
                int(inputs, "reported_term"),
                int(inputs, "leader_term"),
                int(inputs, "leader_base"),
            );
            json!({"next_leadership_term_id": next_leadership_term_id, "next_term_base_log_position": next_term_base_log_position, "next_log_position": next_log_position})
        }
        "replay-end" => {
            let terms: Vec<TermSpan> = inputs["terms"]
                .as_array()
                .expect("terms")
                .iter()
                .map(|t| TermSpan {
                    term: t[0].as_i64().expect("term"),
                    base: t[1].as_i64().expect("base"),
                    end: t[2].as_i64().expect("end"),
                })
                .collect();
            let ReplayEnd { term, end_position } = replay_end(&terms, int(inputs, "position"));
            json!({"term": term, "end_position": end_position})
        }
        "replication-record" => {
            let RecordedTerms {
                replicated,
                leader,
                log_leadership_term_id,
            } = replication_record(
                int(inputs, "term_replicated"),
                int(inputs, "term_base"),
                int(inputs, "stop_position"),
                int(inputs, "leader_term"),
                int(inputs, "leader_base"),
            );
            let mut recorded = vec![json!([replicated.0, replicated.1])];
            if let Some((term, base)) = leader {
                recorded.push(json!([term, base]));
            }
            json!({"recorded": recorded, "log_leadership_term_id": log_leadership_term_id})
        }
        "entry-position" => {
            let after = entry_position(
                int(inputs, "position"),
                usize::try_from(int(inputs, "length")).expect("length"),
                usize::try_from(int(inputs, "mtu")).expect("mtu"),
            )
            .unwrap_or_else(|| panic!("{name}: no position after the entry"));
            json!({"fragments": after.fragments, "position_after": after.position_after})
        }
        "appointed-leader" => {
            let ids: Vec<i32> = inputs["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|id| i32::try_from(id.as_i64().expect("id")).expect("id"))
                .collect();
            json!({"leader": appointed_leader(&ids)})
        }
        "appointed-canvass" => {
            let members = usize::try_from(int(inputs, "members")).expect("members");
            let log = |value: &Value| {
                value.as_array().map(|pair| LogReport {
                    term: pair[0].as_i64().expect("term"),
                    position: pair[1].as_i64().expect("position"),
                })
            };
            let reports: Vec<Option<LogReport>> = inputs["reports"]
                .as_array()
                .expect("reports")
                .iter()
                .map(log)
                .collect();
            let AppointedCanvass {
                leads,
                unanimous,
                ahead,
            } = appointed_canvass(
                members,
                &reports,
                log(&inputs["own"]).expect("own"),
                inputs["deadline_passed"]
                    .as_bool()
                    .expect("deadline_passed"),
            );
            json!({"leads": leads, "unanimous": unanimous, "ahead": ahead})
        }
        "leader-ready" => {
            let count = |name: &str| usize::try_from(int(inputs, name)).expect("a count");
            let closes = leader_ready(
                count("members"),
                count("heard"),
                count("counted"),
                count("ahead"),
                inputs["deadline_passed"]
                    .as_bool()
                    .expect("deadline_passed"),
            );
            json!({"closes": closes})
        }
        other => panic!("{name}: unknown rule {other}"),
    };
    assert_eq!(computed, vector["fields"], "{name}: fields");
}

#[test]
fn every_vector_in_spec_vectors_cluster_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("cluster.json parses");
    assert_eq!(
        doc["version"],
        Value::from(VERSION),
        "the vectors are for the version this crate reads and writes"
    );
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 100,
        "cluster.py writes at least 100 vectors"
    );
    let mut kinds: Map<String, Value> = Map::new();
    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        let kind = vector["kind"].as_str().expect("kind");
        match kind {
            "message" => message_vector(name, vector),
            "members" => members_vector(name, vector),
            "arithmetic" => arithmetic_vector(name, vector),
            other => panic!("{name}: unknown kind {other}"),
        }
        let count = kinds.get(kind).and_then(Value::as_u64).unwrap_or(0);
        kinds.insert(kind.to_string(), Value::from(count + 1));
    }
    assert_eq!(
        kinds.len(),
        3,
        "the three kinds of spec/cluster.md, Vectors"
    );
}

#[test]
fn the_enumerations_round_trip_their_codes() {
    for code in 0..5 {
        assert_eq!(EventCode::from_code(code).map(EventCode::code), Some(code));
    }
    assert_eq!(EventCode::from_code(5), None);
    for code in 0..3 {
        assert_eq!(
            CloseReason::from_code(code).map(CloseReason::code),
            Some(code)
        );
        assert_eq!(
            ClusterAction::from_code(code).map(ClusterAction::code),
            Some(code)
        );
        assert_eq!(
            SnapshotMark::from_code(code).map(SnapshotMark::code),
            Some(code)
        );
    }
    for code in 0..2 {
        assert_eq!(
            ChangeType::from_code(code).map(ChangeType::code),
            Some(code)
        );
    }
    assert_eq!(TimeUnit::from_code(0), Some(TimeUnit::Millis));
    assert_eq!(TimeUnit::from_code(1), None);
}

/// A session event of another major is read, its version intact, so that a client can
/// report the cluster's version to its caller (`spec/cluster.md`, "Sessions"): the
/// field-major check is a consensus message's alone ("What a reader rejects",
/// `version`). No vector of `spec/vectors/cluster.json` pins it yet, since a vector is
/// a change to `spec/` and needs an ADR of its own.
#[test]
fn a_session_event_of_another_major_is_read() {
    for version in [65536, -1, -65536, i32::MIN, i32::MAX] {
        let event = Message::SessionEvent {
            cluster_session_id: -1,
            correlation_id: 41,
            leadership_term_id: 3,
            leader_member_id: 0,
            code: EventCode::Error,
            version,
            leader_heartbeat_timeout_ns: 10_000_000_000,
            detail: b"invalid client version",
        };
        let mut bytes = [0u8; 128];
        let length = event.write(&mut bytes).expect("a session event encodes");
        let decoded = Message::parse(Stream::Egress, &bytes[..length])
            .unwrap_or_else(|error| panic!("version {version}: {error}"));
        assert_eq!(
            decoded, event,
            "version {version}: decoded with the value intact"
        );
        let mut again = [0u8; 128];
        assert_eq!(decoded.write(&mut again), Ok(length));
        assert_eq!(
            again[..length],
            bytes[..length],
            "version {version}: the same bytes"
        );
    }
}

/// The first member of `text` and how many it holds, or the reason it is refused.
fn first_member(text: &str) -> Result<(usize, Member<'_>), &'static str> {
    let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
    parse_members(text.as_bytes(), &mut members, None)
        .map(|count| (count, members[0]))
        .map_err(|error| error.reason())
}

/// A members string's id and port are the integer their ASCII digits make, leading
/// zeros included, as `spec/vectors/cluster.py`'s `parse_members` reads them
/// (`spec/cluster.md`, "What a reader rejects"), so that members and clients built on
/// different implementations agree on one string; a run of digits above the field's
/// range is refused however long it is, and two ids of one value are one id. No vector
/// of `spec/vectors/cluster.json` pins it yet, for the reason above.
#[test]
fn a_members_string_reads_leading_zeros_and_refuses_values_out_of_range() {
    assert_eq!(
        first_member("00000000001,a:1,a:2,a:3,a:4,a:5").map(|(count, m)| (count, m.id)),
        Ok((1, 1)),
        "a leading-zero id"
    );
    assert_eq!(
        first_member("0,a:000080,a:2,a:3,a:4,a:5").map(|(_, m)| m.ingress.port),
        Ok(80),
        "a leading-zero port"
    );
    assert_eq!(
        first_member("0002147483647,[::1]:00000000000000000000065535,a:2,a:3,a:4,a:5")
            .map(|(_, m)| (m.id, m.ingress.port)),
        Ok((i32::MAX, u16::MAX)),
        "the largest id and port, with leading zeros"
    );
    for refused in [
        "2147483648,a:1,a:2,a:3,a:4,a:5",
        "99999999999,a:1,a:2,a:3,a:4,a:5",
        "4294967297,a:1,a:2,a:3,a:4,a:5",
        "100000000000000000000000000000000,a:1,a:2,a:3,a:4,a:5",
        "0,a:65536,a:2,a:3,a:4,a:5",
        "0,a:00000,a:2,a:3,a:4,a:5",
        "0,a:99999999999999999999999999,a:2,a:3,a:4,a:5",
    ] {
        assert_eq!(
            first_member(refused).map(|(count, _)| count),
            Err("members-grammar"),
            "{refused}"
        );
    }
    assert_eq!(
        first_member("1,a:1,a:2,a:3,a:4,a:5|01,b:1,b:2,b:3,b:4,b:5").map(|(count, _)| count),
        Err("members-duplicate"),
        "1 and 01 are one id"
    );
}
