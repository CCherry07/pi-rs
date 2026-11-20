// Harness self-test ONLY. Never accepted as a product/oracle target.
import { StringDecoder } from 'node:string_decoder';
const decoder = new StringDecoder('utf8');
let input = '';
process.stdout.write('{"type":"ready"}\n');
process.on('SIGHUP', () => {
  process.stderr.write('observed SIGHUP\n');
  process.exitCode = 129;
  process.stdin.destroy();
});
process.stdin.on('data', chunk => {
  input += decoder.write(chunk);
  for (let end; (end = input.indexOf('\n')) >= 0;) {
    const command = JSON.parse(input.slice(0, end));
    input = input.slice(end + 1);
    if (command.type === 'invalid-utf8') {
      process.stdout.write(Buffer.concat([Buffer.from('{"type":"bad","value":"'), Buffer.from([255]), Buffer.from('"}\n')]));
    }
    process.stdout.write('{"type":"notice","text":"before response"}\n');
    const bytes = Buffer.from(JSON.stringify({ type: 'response', id: command.id, command: command.type, success: true, data: { value: command.value } }) + '\n');
    for (const byte of bytes) process.stdout.write(Buffer.from([byte]));
  }
});
