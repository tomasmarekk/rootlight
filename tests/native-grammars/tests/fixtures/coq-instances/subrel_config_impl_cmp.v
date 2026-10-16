#[global] Instance subrel_config_impl_cmp cf1 cf2 cs pb :
  config.impl cf1 cf2 ->
  RelationClasses.subrelation (@compare_sort cf1 cs pb) (@compare_sort cf2 cs pb).