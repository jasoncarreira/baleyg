import {validate} from './formats.mjs';
function scalarTable(source) {
  const bytes = Buffer.isBuffer(source) ? source : Buffer.from(source);
  const text = new TextDecoder('utf-8',{fatal:true,ignoreBOM:true}).decode(bytes);
  const tables = {utf8:new Map([[0,0]]),utf16:new Map([[0,0]]),unicodeScalar:new Map([[0,0]])};
  let byte=0,unit=0,scalar=0;
  for (const char of text) {
    byte+=Buffer.byteLength(char); unit+=char.length; scalar++;
    tables.utf8.set(byte,byte); tables.utf16.set(unit,byte); tables.unicodeScalar.set(scalar,byte);
  }
  return {bytes,tables};
}
export function toByteRange(source,range) {
  validate('PositionRange',range);
  const {tables}=scalarTable(source), table=tables[range.encoding];
  if (range.start>range.end || !table.has(range.start) || !table.has(range.end)) throw new RangeError('COORD.INVALID_RANGE invalidRange: offset outside source or scalar boundary');
  return {start:table.get(range.start),end:table.get(range.end)};
}
export function verifyWitness(source,witness,{within=null}={}) {
  validate('SourceWitness',witness);
  const bytes=Buffer.isBuffer(source) ? source : Buffer.from(source);
  const range=toByteRange(bytes,witness.range);
  if (within && (range.start<within.start || range.end>within.end)) throw new RangeError('WITNESS.CONTAINMENT witness outside invocation');
  if (bytes.subarray(range.start,range.end).toString('utf8')!==witness.text) throw new Error('WITNESS.BYTES source spelling differs');
  return range;
}

// A spelling on a call without a measured callee span still needs its own
// exact-source witness. This never synthesizes a calleeRange from that witness.
export function verifyCallSpelling(source,call,witness) {
  if (call.calleeRange !== null || call.spelling === null) throw new TypeError('WITNESS.CALL requires spelling and null calleeRange');
  if (witness == null) throw new Error('WITNESS.MISSING separate spelling witness required');
  if (witness.text !== call.spelling) throw new Error('WITNESS.BYTES source spelling differs');
  return verifyWitness(source,witness,{within:call.range});
}
