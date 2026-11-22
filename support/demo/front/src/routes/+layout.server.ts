import { env } from '$env/dynamic/public';

export const load = () => {
  const publicUrl = (env.PUBLIC_URL ?? '').replace(/\/+$/, '');
  return {
    publicUrl,
    host: publicUrl.replace(/^https?:\/\//, '')
  };
};
