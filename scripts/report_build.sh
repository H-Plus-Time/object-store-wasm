rm -rf report_pkg
mkdir -p report_pkg

echo "Building core"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/core \
  --out-name object_store_wasm \
  --target web \
  --no-default-features \
  --features=js_binding &

echo "Building default"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/default \
  --out-name object_store_wasm \
  --target web &
  
echo "Building aws"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/aws \
  --out-name object_store_wasm \
  --target web \
  --features=aws &

echo "Building gcp"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/gcp \
  --out-name object_store_wasm \
  --target web \
  --features=gcp &

echo "Building azure"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/azure \
  --out-name object_store_wasm \
  --target web \
  --features=azure &

# 2-tuple combinations of cloud providers
echo "Building azure_aws"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/azure_aws \
  --out-name object_store_wasm \
  --target web \
  --features={azure,aws} &

echo "Building azure_gcp"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/azure_gcp \
  --out-name object_store_wasm \
  --target web \
  --features={azure,gcp} &

echo "Building aws_gcp"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/aws_gcp \
  --out-name object_store_wasm \
  --target web \
  --features={aws,gcp} &

echo "Building full"
wasm-pack build \
  --release \
  --no-pack \
  --out-dir report_pkg/full \
  --out-name object_store_wasm \
  --target web \
  --features=full &

wait;