// 按扩展名推断 mime（data: URI 显示需要）
export function mimeForPath(path: string): string {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  switch (ext) {
    case 'png': return 'image/png';
    case 'jpg':
    case 'jpeg': return 'image/jpeg';
    case 'gif': return 'image/gif';
    case 'webp': return 'image/webp';
    case 'svg': return 'image/svg+xml';
    case 'bmp': return 'image/bmp';
    case 'ico': return 'image/x-icon';
    case 'xpm': return 'image/x-xpixmap';
    case 'tif':
    case 'tiff': return 'image/tiff';
    default: return 'application/octet-stream';
  }
}
