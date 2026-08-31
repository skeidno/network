-keep class io.nekohasekai.libbox.** { *; }
-keep class go.** { *; }

# SnakeYAML probes java.beans when it runs on a desktop JRE. Android does not
# provide those optional introspection classes and our parser does not use them.
-dontwarn java.beans.BeanInfo
-dontwarn java.beans.FeatureDescriptor
-dontwarn java.beans.IntrospectionException
-dontwarn java.beans.Introspector
-dontwarn java.beans.PropertyDescriptor
