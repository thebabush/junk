"""Tests for tools/pklg2trace.py, driven by synthetic .pklg bytes."""

import struct
import uuid
from pathlib import Path
from zoneinfo import ZoneInfo

import pklg2trace
import pytest
from pklg2trace import HandleOverride, PklgError, RecordKind

NEW_YORK = ZoneInfo('America/New_York')
EPOCH = 1_763_547_645  # wall clock 2025-11-19T10:20:45, stored the way PacketLogger does (as if UTC)
CONNECTION = 0x005b
FRAME_77 = bytes.fromhex('7701070000000000000000000000007f')


def record(kind: int, payload: bytes, micros: int = 0, byte_order: str = '<') -> bytes:
    return struct.pack(f'{byte_order}IIIB', 9 + len(payload), EPOCH, micros, kind) + payload


def acl(pb_flag: int, data: bytes, connection: int = CONNECTION) -> bytes:
    return struct.pack('<HH', connection | (pb_flag << 12), len(data)) + data


def att_frame(pdu: bytes) -> bytes:
    return struct.pack('<HH', len(pdu), 0x0004) + pdu


def notification(handle: int, value: bytes) -> bytes:
    return bytes([0x1b]) + struct.pack('<H', handle) + value


def write_request(handle: int, value: bytes) -> bytes:
    return bytes([0x12]) + struct.pack('<H', handle) + value


def received(pdu: bytes, micros: int = 0) -> bytes:
    return record(RecordKind.ACL_RECEIVED, acl(0b10, att_frame(pdu)), micros)


def sent(pdu: bytes, micros: int = 0) -> bytes:
    return record(RecordKind.ACL_SENT, acl(0b00, att_frame(pdu)), micros)


def hci_event(code: int, params: bytes) -> bytes:
    return record(RecordKind.HCI_EVENT, bytes([code, len(params)]) + params)


def convert(data: bytes, overrides: list[HandleOverride] | None = None) -> str:
    return pklg2trace.convert(data, 'synthetic.pklg', 'synthetic', NEW_YORK, overrides or [])


def data_lines(text: str) -> list[str]:
    return [line for line in text.splitlines() if not line.startswith(('#', '!'))]


def event_lines(text: str) -> list[str]:
    return [line for line in text.splitlines() if line.startswith('! ')]


def test_le_and_be_headers_parse_to_the_same_records() -> None:
    pdu = notification(0x0012, bytes(16))
    little = record(RecordKind.ACL_RECEIVED, acl(0b10, att_frame(pdu)), micros=341000)
    big = record(RecordKind.ACL_RECEIVED, acl(0b10, att_frame(pdu)), micros=341000, byte_order='>')
    assert little != big
    assert little[:4] == big[:4][::-1] == struct.pack('<I', len(little) - 4)
    assert pklg2trace.parse_pklg(little) == pklg2trace.parse_pklg(big)
    assert pklg2trace.parse_pklg(little)[0].kind == RecordKind.ACL_RECEIVED
    assert data_lines(convert(big)) == [f'2025-11-19T10:20:45.341-05:00 rx v1.notify {bytes(16).hex()}']


def test_truncated_file_is_an_error_not_a_traceback(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    data = received(notification(0x0012, bytes(16))) + sent(write_request(0x0010, FRAME_77))
    with pytest.raises(PklgError):
        pklg2trace.parse_pklg(data[:-5])
    with pytest.raises(PklgError):
        pklg2trace.parse_pklg(b'')
    path = tmp_path / 'broken.pklg'
    path.write_bytes(data[:-5])
    assert pklg2trace.main([str(path)]) == 1
    captured = capsys.readouterr()
    assert captured.out == ''
    assert 'pklg2trace: error:' in captured.err and 'broken.pklg' in captured.err


def test_notification_split_across_fragments_reassembles() -> None:
    value = bytes(range(40))
    frame = att_frame(notification(0x0012, value))
    first = record(RecordKind.ACL_RECEIVED, acl(0b10, frame[:20]), micros=100000)
    rest = record(RecordKind.ACL_RECEIVED, acl(0b01, frame[20:]), micros=120000)
    text = convert(first + rest)
    assert data_lines(text) == [f'2025-11-19T10:20:45.120-05:00 rx v1.notify {value.hex()}']


def test_fragments_are_kept_apart_per_direction() -> None:
    rx_frame = att_frame(notification(0x0012, bytes([0xaa] * 30)))
    tx_frame = att_frame(write_request(0x0010, bytes([0xbb] * 30)))
    data = (
        record(RecordKind.ACL_RECEIVED, acl(0b10, rx_frame[:10]), micros=1000)
        + record(RecordKind.ACL_SENT, acl(0b00, tx_frame[:10]), micros=2000)
        + record(RecordKind.ACL_SENT, acl(0b01, tx_frame[10:]), micros=3000)
        + record(RecordKind.ACL_RECEIVED, acl(0b01, rx_frame[10:]), micros=4000)
    )
    assert data_lines(convert(data)) == [
        f'2025-11-19T10:20:45.003-05:00 tx v1.write {bytes([0xbb] * 30).hex()}',
        f'2025-11-19T10:20:45.004-05:00 rx v1.notify {bytes([0xaa] * 30).hex()}',
    ]


def test_orphan_continuation_is_dropped_not_crashed_on(capsys: pytest.CaptureFixture[str]) -> None:
    orphan = record(RecordKind.ACL_RECEIVED, acl(0b01, b'\x01\x02\x03'))
    text = convert(orphan + received(notification(0x0012, b'\x2f\xf4'), micros=500000))
    assert data_lines(text) == ['2025-11-19T10:20:45.500-05:00 rx v1.notify 2ff4']
    assert event_lines(text) == []
    assert 'continuation fragment on handle 0x005b received with no frame open, dropped' in capsys.readouterr().err


def test_discovery_maps_value_handle_and_override_beats_it() -> None:
    uuid_le = uuid.UUID('de5bf729-d711-4e47-af26-65e3012a5dc7').bytes[::-1]
    declaration = struct.pack('<HBH', 0x0020, 0x10, 0x0021) + uuid_le
    read_by_type = bytes([0x09, 21]) + declaration
    data = received(read_by_type) + received(notification(0x0021, b'\xbc\x42'), micros=216000)

    text = convert(data)
    assert '# handle map: 0x0021=v2.notify   (derived from discovery)' in text
    assert data_lines(text) == ['2025-11-19T10:20:45.216-05:00 rx v2.notify bc42']

    text = convert(data, overrides=[HandleOverride(0x0021, 'v1.notify')])
    assert '# handle map: 0x0021=v1.notify   (overrides over discovery)' in text
    assert data_lines(text) == ['2025-11-19T10:20:45.216-05:00 rx v1.notify bc42']


def test_discovery_of_foreign_16bit_uuids_falls_back_to_defaults() -> None:
    declaration = struct.pack('<HBH', 0x0002, 0x02, 0x0003) + (0x2a00).to_bytes(2, 'little')
    text = convert(received(bytes([0x09, 7]) + declaration))
    assert '# handle map: 0x0010=v1.write 0x0012=v1.notify 0x0016=v2.cmd 0x0018=v2.notify   (defaults)' in text


def test_override_without_discovery_layers_over_defaults() -> None:
    text = convert(received(notification(0x0012, b'\x01')), overrides=[HandleOverride(0x0030, 'v2.cmd')])
    assert '0x0012=v1.notify' in text and '0x0030=v2.cmd   (overrides over defaults)' in text


def test_unmapped_handle_becomes_orientation_line(capsys: pytest.CaptureFixture[str]) -> None:
    text = convert(sent(write_request(0x0013, b'\x00\x00'), micros=632000))
    assert data_lines(text) == []
    assert event_lines(text) == ['! 2025-11-19T10:20:45.632-05:00 att write handle=0x0013 value=0000']
    assert '1 value(s) on 1 unmapped handle(s): 0x0013' in capsys.readouterr().err


def test_read_response_is_attributed_to_the_pending_read_request() -> None:
    read_request = bytes([0x0a]) + struct.pack('<H', 0x0012)
    read_response = bytes([0x0b]) + b'\x2f\xf4'
    text = convert(sent(read_request) + received(read_response, micros=7000))
    assert data_lines(text) == ['2025-11-19T10:20:45.007-05:00 rx v1.notify 2ff4']


def test_link_events_become_orientation_lines() -> None:
    connected = hci_event(0x3e, bytes([0x01, 0x00]) + struct.pack('<H', CONNECTION) + bytes(15))
    mtu = sent(bytes([0x02]) + struct.pack('<H', 247)) + received(bytes([0x03]) + struct.pack('<H', 512))
    failed_disconnect = hci_event(0x05, bytes([0x02]) + struct.pack('<H', CONNECTION) + bytes([0x13]))
    disconnected = hci_event(0x05, bytes([0x00]) + struct.pack('<H', CONNECTION) + bytes([0x13]))
    text = convert(connected + mtu + failed_disconnect + disconnected)
    assert event_lines(text) == [
        '! 2025-11-19T10:20:45.000-05:00 connected handle=0x005b',
        '! 2025-11-19T10:20:45.000-05:00 mtu 247',
        '! 2025-11-19T10:20:45.000-05:00 disconnected reason=0x13',
    ]
    assert data_lines(text) == []


def test_unwritable_output_is_an_error_not_a_traceback(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    path = tmp_path / 'session.pklg'
    path.write_bytes(sent(write_request(0x0010, FRAME_77)))
    assert pklg2trace.main(['-o', str(tmp_path / 'missing' / 'out.trace'), str(path)]) == 1
    captured = capsys.readouterr()
    assert captured.out == ''
    assert 'pklg2trace: error:' in captured.err


def test_direction_and_opcode_disagreement_warns(capsys: pytest.CaptureFixture[str]) -> None:
    text = convert(received(write_request(0x0010, b'\x01')))
    assert data_lines(text) == ['2025-11-19T10:20:45.000-05:00 rx v1.write 01']
    assert 'ATT opcode 0x12 was rx but its opcode implies tx' in capsys.readouterr().err


def test_timestamp_uses_requested_zone(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    path = tmp_path / 'session.pklg'
    path.write_bytes(sent(write_request(0x0010, FRAME_77), micros=341000))

    assert pklg2trace.main(['--tz', 'America/New_York', str(path)]) == 0
    out = capsys.readouterr().out
    assert f'2025-11-19T10:20:45.341-05:00 tx v1.write {FRAME_77.hex()}' in out
    assert out.startswith('# junk trace v1 — session.pklg\n# source: session.pklg, converted by tools/pklg2trace.py\n')

    output = tmp_path / 'session.trace'
    assert pklg2trace.main(['--tz', 'UTC', '--desc', 'live HR', '-o', str(output), str(path)]) == 0
    assert capsys.readouterr().out == ''
    text = output.read_text(encoding='utf-8')
    assert text.startswith('# junk trace v1 — live HR\n')
    assert f'2025-11-19T10:20:45.341+00:00 tx v1.write {FRAME_77.hex()}' in text
