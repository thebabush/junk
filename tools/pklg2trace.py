#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Convert an Apple PacketLogger .pklg capture into the junk trace v1 text format.

Only ATT traffic survives: writes, notifications, indications and read responses on the
known Colmi characteristics become data lines, everything else on the link is dropped
except the connection, disconnection and MTU events that are kept as "! " orientation
lines. Values on handles outside the map are kept as orientation lines too, never dropped.

PacketLogger stamps each record with the capturing device's wall-clock time stored as if it
were UTC. Verified on the capture behind fixtures/colmi-r10/thering-realtime-2025-11-19.trace (not
published): its mtime is 31 s after the last record when the stamps are read as UTC-5
wall-clock, and five hours after when read as UTC. Stamps are therefore read as naive wall-clock times and
get the --tz zone attached, which is the zone the capture was taken in, not a display zone.
If a capture from another source turns out to hold true UTC, pass --tz UTC.
"""

import argparse
import struct
import sys
import uuid
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timedelta, tzinfo
from enum import IntEnum
from pathlib import Path
from typing import Literal, assert_never
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

type Direction = Literal['tx', 'rx']
type ValueKind = Literal['write', 'notify', 'read']

# Characteristic UUID → trace channel, from docs/colmi-protocol.md.
CHANNEL_UUIDS: dict[str, str] = {
    '6e400002-b5a3-f393-e0a9-e50e24dcca9e': 'v1.write',
    '6e400003-b5a3-f393-e0a9-e50e24dcca9e': 'v1.notify',
    'de5bf72a-d711-4e47-af26-65e3012a5dc7': 'v2.cmd',
    'de5bf729-d711-4e47-af26-65e3012a5dc7': 'v2.notify',
}
CHANNELS = frozenset(CHANNEL_UUIDS.values())

# Attribute handles the Colmi R10 hands out, used when the capture holds no discovery.
DEFAULT_HANDLE_MAP: dict[int, str] = {
    0x0010: 'v1.write',
    0x0012: 'v1.notify',
    0x0016: 'v2.cmd',
    0x0018: 'v2.notify',
}

BLUETOOTH_BASE_UUID = uuid.UUID('00000000-0000-1000-8000-00805f9b34fb')
L2CAP_CID_ATT = 0x0004
PKLG_HEADER_SIZE = 13


class RecordKind(IntEnum):
    HCI_COMMAND = 0x00
    HCI_EVENT = 0x01
    ACL_SENT = 0x02
    ACL_RECEIVED = 0x03


class HciEvent(IntEnum):
    DISCONNECTION_COMPLETE = 0x05
    LE_META = 0x3e


class LeSubevent(IntEnum):
    CONNECTION_COMPLETE = 0x01
    ENHANCED_CONNECTION_COMPLETE = 0x0a


class AttOpcode(IntEnum):
    EXCHANGE_MTU_REQUEST = 0x02
    EXCHANGE_MTU_RESPONSE = 0x03
    READ_BY_TYPE_RESPONSE = 0x09
    READ_REQUEST = 0x0a
    READ_RESPONSE = 0x0b
    WRITE_REQUEST = 0x12
    HANDLE_VALUE_NOTIFICATION = 0x1b
    HANDLE_VALUE_INDICATION = 0x1d
    WRITE_COMMAND = 0x52


class PklgError(Exception):
    """The input is not a PacketLogger file this tool can walk."""


@dataclass(frozen=True)
class PklgRecord:
    timestamp: datetime
    kind: int
    payload: bytes


@dataclass(frozen=True)
class L2capFrame:
    timestamp: datetime
    connection: int
    sent: bool
    cid: int
    payload: bytes


@dataclass(frozen=True)
class AttEvent:
    timestamp: datetime
    connection: int
    sent: bool
    opcode: int
    params: bytes


@dataclass(frozen=True)
class AttValue:
    """A value moving over a handle, with the direction its opcode implies."""

    kind: ValueKind
    direction: Direction
    handle: int
    value: bytes


@dataclass(frozen=True)
class DataLine:
    timestamp: datetime
    direction: Direction
    channel: str
    value: bytes


@dataclass(frozen=True)
class EventLine:
    timestamp: datetime
    text: str


@dataclass(frozen=True)
class HandleOverride:
    handle: int
    channel: str


@dataclass
class PartialFrame:
    expected: int
    data: bytearray


def warn(message: str) -> None:
    print(f'pklg2trace: {message}', file=sys.stderr)


def parse_pklg(data: bytes) -> list[PklgRecord]:
    """Split a .pklg file into records, detecting the byte order of the record headers."""
    if not data:
        raise PklgError('empty file')
    for byte_order in ('<', '>'):
        records = walk_records(data, byte_order)
        if records is not None:
            return records
    raise PklgError('not a PacketLogger file: record lengths do not add up in either byte order')


def walk_records(data: bytes, byte_order: str) -> list[PklgRecord] | None:
    """Walk records with the given header byte order; None if they do not tile the file."""
    header = struct.Struct(f'{byte_order}IIIB')
    records: list[PklgRecord] = []
    offset = 0
    while offset < len(data):
        if offset + PKLG_HEADER_SIZE > len(data):
            return None
        length, seconds, micros, kind = header.unpack_from(data, offset)
        end = offset + 4 + length
        if length < PKLG_HEADER_SIZE - 4 or end > len(data):
            return None
        # Deliberately naive: the stamp is the device's wall clock, the zone is attached when rendering.
        timestamp = datetime(1970, 1, 1) + timedelta(seconds=seconds, microseconds=micros)  # noqa: DTZ001
        records.append(PklgRecord(timestamp, kind, data[offset + PKLG_HEADER_SIZE:end]))
        offset = end
    return records


class AclReassembler:
    """Joins ACL fragments into whole L2CAP frames, per connection handle and direction."""

    def __init__(self) -> None:
        self._partial: dict[tuple[int, bool], PartialFrame] = {}

    def feed(self, record: PklgRecord) -> L2capFrame | None:
        """Absorb one ACL record; returns the L2CAP frame it completes, stamped with that record's time."""
        if len(record.payload) < 4:
            warn('ACL record shorter than its header, dropped')
            return None
        handle_flags, data_len = struct.unpack_from('<HH', record.payload)
        connection = handle_flags & 0x0fff
        pb_flag = (handle_flags >> 12) & 0b11
        sent = record.kind == RecordKind.ACL_SENT
        data = record.payload[4:4 + data_len]
        key = (connection, sent)
        where = f'handle 0x{connection:04x} {"sent" if sent else "received"}'
        match pb_flag:
            case 0b00 | 0b10:
                if key in self._partial:
                    warn(f'new L2CAP frame on {where} while one was still incomplete, old one dropped')
                if len(data) < 4:
                    warn(f'first fragment on {where} shorter than an L2CAP header, dropped')
                    self._partial.pop(key, None)
                    return None
                l2cap_len = struct.unpack_from('<H', data)[0]
                self._partial[key] = PartialFrame(l2cap_len + 4, bytearray(data))
            case 0b01:
                partial = self._partial.get(key)
                if partial is None:
                    warn(f'continuation fragment on {where} with no frame open, dropped')
                    return None
                partial.data += data
            case _:
                warn(f'ACL fragment on {where} with reserved PB flag {pb_flag:#04b}, dropped')
                return None
        partial = self._partial[key]
        if len(partial.data) < partial.expected:
            return None
        del self._partial[key]
        cid = struct.unpack_from('<H', partial.data, 2)[0]
        return L2capFrame(record.timestamp, connection, sent, cid, bytes(partial.data[4:partial.expected]))


def link_event(record: PklgRecord) -> EventLine | None:
    """Turn the few HCI events worth keeping into orientation lines."""
    if len(record.payload) < 2:
        return None
    code, length = record.payload[0], record.payload[1]
    params = record.payload[2:2 + length]
    match code:
        case HciEvent.DISCONNECTION_COMPLETE if len(params) >= 4:
            status, _handle, reason = struct.unpack_from('<BHB', params)
            if status == 0:
                return EventLine(record.timestamp, f'disconnected reason=0x{reason:02x}')
        case HciEvent.LE_META if len(params) >= 4:
            subevent, status, handle = struct.unpack_from('<BBH', params)
            if subevent in (LeSubevent.CONNECTION_COMPLETE, LeSubevent.ENHANCED_CONNECTION_COMPLETE) and status == 0:
                return EventLine(record.timestamp, f'connected handle=0x{handle:04x}')
    return None


def collect_events(records: list[PklgRecord]) -> list[AttEvent | EventLine]:
    """Reduce the record stream to ATT PDUs and link events, in capture order."""
    reassembler = AclReassembler()
    events: list[AttEvent | EventLine] = []
    for record in records:
        match record.kind:
            case RecordKind.HCI_EVENT:
                line = link_event(record)
                if line is not None:
                    events.append(line)
            case RecordKind.ACL_SENT | RecordKind.ACL_RECEIVED:
                frame = reassembler.feed(record)
                if frame is not None and frame.cid == L2CAP_CID_ATT and frame.payload:
                    events.append(
                        AttEvent(frame.timestamp, frame.connection, frame.sent, frame.payload[0], frame.payload[1:])
                    )
    return events


def uuid_from_le(raw: bytes) -> str:
    """Render a little-endian 16-bit or 128-bit UUID the way the channel table spells it."""
    if len(raw) == 2:
        return str(uuid.UUID(int=BLUETOOTH_BASE_UUID.int | (int.from_bytes(raw, 'little') << 96)))
    return str(uuid.UUID(bytes=raw[::-1]))


def discovered_handle_map(events: list[AttEvent | EventLine]) -> dict[int, str]:
    """Map value handles to channels from the characteristic declarations in Read By Type Responses."""
    mapping: dict[int, str] = {}
    for event in events:
        if not isinstance(event, AttEvent) or event.opcode != AttOpcode.READ_BY_TYPE_RESPONSE or not event.params:
            continue
        entry_len = event.params[0]
        if entry_len not in (7, 21):
            continue
        entries = event.params[1:]
        for offset in range(0, len(entries) - entry_len + 1, entry_len):
            _handle, _properties, value_handle = struct.unpack_from('<HBH', entries, offset)
            channel = CHANNEL_UUIDS.get(uuid_from_le(entries[offset + 5:offset + entry_len]))
            if channel is not None:
                mapping[value_handle] = channel
    return mapping


def resolve_handle_map(overrides: list[HandleOverride], discovered: dict[int, str]) -> tuple[dict[int, str], str]:
    """Pick the handle map: CLI overrides beat discovery, which beats the built-in defaults."""
    base, source = (discovered, 'derived from discovery') if discovered else (DEFAULT_HANDLE_MAP, 'defaults')
    if not overrides:
        return dict(base), source
    mapping = dict(base)
    for override in overrides:
        mapping[override.handle] = override.channel
    return mapping, f'overrides over {"discovery" if discovered else "defaults"}'


def att_value(event: AttEvent, pending_reads: dict[int, int]) -> AttValue | None:
    """Extract the handle and value carried by an ATT PDU, or None if it carries none."""
    match event.opcode:
        case AttOpcode.WRITE_REQUEST | AttOpcode.WRITE_COMMAND if len(event.params) >= 2:
            return AttValue('write', 'tx', struct.unpack_from('<H', event.params)[0], event.params[2:])
        case AttOpcode.HANDLE_VALUE_NOTIFICATION | AttOpcode.HANDLE_VALUE_INDICATION if len(event.params) >= 2:
            return AttValue('notify', 'rx', struct.unpack_from('<H', event.params)[0], event.params[2:])
        case AttOpcode.READ_REQUEST if len(event.params) >= 2:
            pending_reads[event.connection] = struct.unpack_from('<H', event.params)[0]
        case AttOpcode.READ_RESPONSE:
            handle = pending_reads.pop(event.connection, None)
            if handle is None:
                warn(f'read response on handle 0x{event.connection:04x} without a read request, dropped')
                return None
            return AttValue('read', 'rx', handle, event.params)
    return None


def build_lines(
    events: list[AttEvent | EventLine], handle_map: dict[int, str]
) -> tuple[list[DataLine | EventLine], Counter[int]]:
    """Turn the event stream into trace lines; also count values seen on unmapped handles."""
    lines: list[DataLine | EventLine] = []
    pending_reads: dict[int, int] = {}
    requested_mtu: dict[int, int] = {}
    unmapped: Counter[int] = Counter()
    for event in events:
        if isinstance(event, EventLine):
            lines.append(event)
            continue
        # The negotiated MTU is the smaller of what the client asked for and what the server allows.
        if event.opcode == AttOpcode.EXCHANGE_MTU_REQUEST and len(event.params) >= 2:
            requested_mtu[event.connection] = struct.unpack_from('<H', event.params)[0]
            continue
        if event.opcode == AttOpcode.EXCHANGE_MTU_RESPONSE and len(event.params) >= 2:
            server_mtu = struct.unpack_from('<H', event.params)[0]
            mtu = min(server_mtu, requested_mtu.pop(event.connection, server_mtu))
            lines.append(EventLine(event.timestamp, f'mtu {mtu}'))
            continue
        value = att_value(event, pending_reads)
        if value is None:
            continue
        direction: Direction = 'tx' if event.sent else 'rx'
        if direction != value.direction:
            warn(f'ATT opcode 0x{event.opcode:02x} was {direction} but its opcode implies {value.direction}')
        channel = handle_map.get(value.handle)
        if channel is None:
            unmapped[value.handle] += 1
            text = f'att {value.kind} handle=0x{value.handle:04x} value={value.value.hex()}'
            lines.append(EventLine(event.timestamp, text))
        else:
            lines.append(DataLine(event.timestamp, direction, channel, value.value))
    return lines, unmapped


def render(line: DataLine | EventLine, tz: tzinfo | None) -> str:
    # The stored stamp is a naive wall-clock time; None means the system local zone.
    stamped = line.timestamp.astimezone() if tz is None else line.timestamp.replace(tzinfo=tz)
    stamp = stamped.isoformat(timespec='milliseconds')
    match line:
        case DataLine(direction=direction, channel=channel, value=value):
            return f'{stamp} {direction} {channel} {value.hex()}'
        case EventLine(text=text):
            return f'! {stamp} {text}'
        case _:
            assert_never(line)


def convert(data: bytes, source: str, description: str, tz: tzinfo | None, overrides: list[HandleOverride]) -> str:
    """Convert raw .pklg bytes into the full trace text, header included."""
    events = collect_events(parse_pklg(data))
    handle_map, map_source = resolve_handle_map(overrides, discovered_handle_map(events))
    lines, unmapped = build_lines(events, handle_map)
    if unmapped:
        handles = ' '.join(f'0x{handle:04x}' for handle in sorted(unmapped))
        warn(f'{sum(unmapped.values())} value(s) on {len(unmapped)} unmapped handle(s): {handles}')
    map_text = ' '.join(f'0x{handle:04x}={channel}' for handle, channel in sorted(handle_map.items()))
    header = [
        f'# junk trace v1 — {description}',
        f'# source: {source}, converted by tools/pklg2trace.py',
        '# columns: <iso-ts> <tx|rx> <channel> <hex>',
        f'# handle map: {map_text}   ({map_source})',
        '# lines starting with # are comments; "! " lines are app-side/link events kept for orientation',
    ]
    return '\n'.join(header + [render(line, tz) for line in lines]) + '\n'


def parse_zone(name: str) -> ZoneInfo:
    try:
        return ZoneInfo(name)
    except (ZoneInfoNotFoundError, ValueError):
        raise argparse.ArgumentTypeError(f'unknown time zone {name!r}') from None


def parse_override(text: str) -> HandleOverride:
    handle_text, separator, channel = text.partition('=')
    if not separator or channel not in CHANNELS:
        names = ', '.join(sorted(CHANNELS))
        raise argparse.ArgumentTypeError(f'expected HANDLE=CHANNEL with CHANNEL one of {names}, got {text!r}')
    try:
        return HandleOverride(int(handle_text, 0), channel)
    except ValueError:
        raise argparse.ArgumentTypeError(f'bad handle {handle_text!r} in {text!r}') from None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description='Convert a PacketLogger .pklg capture into a junk trace v1 file.')
    parser.add_argument('pklg', type=Path, help='PacketLogger capture to convert')
    parser.add_argument('-o', '--output', type=Path, help='write the trace here instead of stdout')
    parser.add_argument('--desc', help='description for the first header line (default: the pklg basename)')
    parser.add_argument('--tz', type=parse_zone,
                        help='IANA zone the capture was taken in (default: the system local zone)')
    parser.add_argument('--map', action='append', type=parse_override, default=[], metavar='HANDLE=CHANNEL',
                        help='force an attribute handle onto a channel, e.g. 0x0010=v1.write; repeatable')
    args = parser.parse_args(argv)
    try:
        text = convert(args.pklg.read_bytes(), args.pklg.name, args.desc or args.pklg.name, args.tz, args.map)
    except (OSError, PklgError) as error:
        print(f'pklg2trace: error: {args.pklg}: {error}', file=sys.stderr)
        return 1
    try:
        if args.output is None:
            sys.stdout.write(text)
        else:
            args.output.write_text(text, encoding='utf-8')
    except OSError as error:
        print(f'pklg2trace: error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
