<?php

require_once __DIR__ . '/src/Helper.php';

use App\Helper;

$helper = new Helper();
echo $helper->greet("world");
