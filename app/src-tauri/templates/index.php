<?php
// Default start page created by Stackyard. Feel free to replace it.
if (isset($_GET['phpinfo'])) {
    phpinfo();
    exit;
}
$projects = array_filter(glob(__DIR__ . '/*'), 'is_dir');
?>
<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Stackyard</title>
    <style>
        body { font-family: "Segoe UI", system-ui, sans-serif; max-width: 720px; margin: 48px auto; padding: 0 16px; color: #1c2027; }
        h1 { margin-bottom: 4px; }
        p { color: #6b7380; }
        ul { padding-left: 18px; }
        a { color: #2f6fde; }
    </style>
</head>
<body>
    <h1>Stackyard is running</h1>
    <p>PHP <?= PHP_VERSION ?> &middot; <?= htmlspecialchars($_SERVER['SERVER_SOFTWARE'] ?? '') ?> &middot; <a href="?phpinfo=1">phpinfo()</a></p>
    <h2>Projects in www/</h2>
    <?php if (!$projects): ?>
        <p>No projects yet. Create a folder in <code><?= htmlspecialchars(__DIR__) ?></code>.</p>
    <?php else: ?>
        <ul>
            <?php foreach ($projects as $dir): $name = basename($dir); ?>
                <li><a href="/<?= rawurlencode($name) ?>/"><?= htmlspecialchars($name) ?></a></li>
            <?php endforeach; ?>
        </ul>
    <?php endif; ?>
</body>
</html>
