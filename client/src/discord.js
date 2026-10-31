/** Discord adds `frame_id` to the URL when it loads us as an Activity. */
export const inDiscord = () => new URLSearchParams(location.search).has('frame_id');

/** Inside Discord every request to our own server goes through its proxy. */
export const apiBase = () => (inDiscord() ? '/.proxy' : '');

export async function setupDiscord() {
  const clientId = import.meta.env.VITE_DISCORD_CLIENT_ID;
  if (!clientId) throw new Error('VITE_DISCORD_CLIENT_ID is not set');

  // Only pulled in when running inside Discord.
  const { DiscordSDK } = await import('@discord/embedded-app-sdk');
  const sdk = new DiscordSDK(clientId);
  await sdk.ready();

  const { code } = await sdk.commands.authorize({
    client_id: clientId,
    response_type: 'code',
    state: '',
    prompt: 'none',
    scope: ['identify'],
  });

  const res = await fetch(`${apiBase()}/api/token`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ code }),
  });
  if (!res.ok) throw new Error(`Token exchange failed (${res.status})`);
  const { access_token } = await res.json();

  const { user } = await sdk.commands.authenticate({ access_token });
  return {
    sdk,
    user,
    name: user.global_name || user.username,
    channelId: sdk.channelId,
  };
}
