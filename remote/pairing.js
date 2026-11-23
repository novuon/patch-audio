// Each device pairs once: the code stays in this browser's localStorage.
let pairingPrompted = false;
function pairingStore(code) {
  try {
    if (code === undefined) return localStorage.getItem('patch-pairing') || sessionStorage.getItem('patch-pairing');
    if (code === null) localStorage.removeItem('patch-pairing'); else localStorage.setItem('patch-pairing', code);
  } catch (_) { return null; }
  return null;
}
function patchHeaders(headers) {
  const next = Object.assign({}, headers || {});
  const code = pairingStore();
  if (code) next['X-Patch-Pairing'] = code;
  return next;
}
async function patchFetch(url, options) {
  options = options || {};
  const send = () => fetch(url, Object.assign({}, options, {headers: patchHeaders(options.headers)}));
  const response = await send();
  if (response.status !== 401 || pairingPrompted) return response;
  pairingPrompted = true;
  const entered = window.prompt('Enter the pairing code from the computer running Patch (~/.config/patch-remote/pairing).');
  if (!entered || !entered.trim()) return response;
  pairingStore(entered.trim());
  const retry = await send();
  if (retry.status === 401) pairingStore(null);
  return retry;
}
