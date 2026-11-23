// Imported first: Strudel logs while loading, and stdout is the protocol.
console.log = (...a) => process.stderr.write(a.join(' ') + '\n');
console.info = console.log;
console.warn = console.log;
